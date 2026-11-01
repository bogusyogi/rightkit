#![cfg_attr(not(target_os = "windows"), allow(dead_code, unused_imports))]
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    time::Instant,
};

#[derive(Deserialize)]
struct Input {
    id: String,
    audio_path: String,
    sha256: String,
    duration_s: f64,
}

#[derive(Serialize)]
struct Output<'a> {
    id: &'a str,
    transcript: String,
    status: &'static str,
    error: Option<String>,
    run: &'a str,
    model: &'a str,
    backend: &'a str,
    input_sha256: &'a str,
    duration_s: f64,
    load_ms: Option<f64>,
    mel_ms: Option<f64>,
    encode_ms: Option<f64>,
    decode_ms: Option<f64>,
    total_ms: Option<f64>,
}

fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    // Published owner (rightkit-audio) replaced the hound reader in the port.
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != 16_000 {
        return Err(format!(
            "{}: expected 16 kHz mono WAV, got {} Hz",
            path.display(),
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("Windows only");
    std::process::exit(2);
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("rightkit_asr_stage_timing=info,rightkit_dictation_inference::whisper_win::timing=info")))
        .with_ansi(false)
        .without_time()
        .with_writer(std::io::stderr)
        .try_init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        eprintln!("usage: runner <parakeet|whisper> <input.jsonl> <output.jsonl> <models-dir> <language> <run-state>");
        std::process::exit(2);
    }
    let kind = args[1].as_str();
    let run_state = args[6].as_str();
    let inputs: Vec<Input> = BufReader::new(File::open(&args[2])?)
        .lines()
        .map(|line| serde_json::from_str(&line?).map_err(std::io::Error::other))
        .collect::<Result<_, _>>()?;
    let out = File::create(&args[3])?;
    let mut out = BufWriter::new(out);
    let load_start = Instant::now();
    let mut parakeet = if kind == "parakeet" {
        Some(
            rightkit_dictation_inference::asr::AsrRuntime::load(
                Path::new(&args[4]),
                rightkit_dictation_inference::asr::AsrEp::Dml,
            )
            .map_err(std::io::Error::other)?,
        )
    } else {
        None
    };
    let whisper = if kind == "whisper" {
        Some(
            rightkit_dictation_inference::whisper_win::WhisperWin::load(Path::new(&args[4]))
                .map_err(std::io::Error::other)?,
        )
    } else {
        None
    };
    let model_load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let model = if kind == "parakeet" {
        "parakeet_tdt_v3_static1500_qint8_20260722@f73e8671086bfe369e263468a91826be24cd59d826851ae531dddd5033c4b99e"
    } else {
        "whisper_turbo_q5_k.bin@fcacbe37c8ddd49a137b3ef7cccd2743bea1f9a9dd20d2b9755978972a02bb0b"
    };
    let backend = if kind == "parakeet" {
        "rightkit-inference/AsrRuntime/DML/beam2"
    } else {
        "rightkit-inference/WhisperWin/FFI-v1.9.3; CLI-fallback"
    };

    // One unscored startup pass per fresh process. WhisperWin initializes its
    // resident FFI lazily on this call; startup_ms captures that separately.
    let startup_ms = if let Some(first) = inputs.first() {
        let samples = read_wav(Path::new(&first.audio_path)).map_err(std::io::Error::other)?;
        eprintln!("asr_row_boundary phase=warmup id={} marker=begin", first.id);
        let t = Instant::now();
        let warmup_result: Result<(), std::io::Error> = if let Some(engine) = parakeet.as_mut() {
            engine
                .transcribe_final_result_under_lease(&samples)
                .map(|r| r.text)
                .map(|_| ())
                .map_err(std::io::Error::other)
        } else {
            whisper
                .as_ref()
                .unwrap()
                .transcribe_lang(&samples, &args[5])
                .map(|_| ())
                .map_err(std::io::Error::other)
        };
        let elapsed = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "asr_row_boundary phase=warmup id={} marker=end outer_call_ms={elapsed:.3}",
            first.id
        );
        warmup_result?;
        elapsed
    } else {
        0.0
    };
    eprintln!(
        "startup_ms={startup_ms:.3}; model_load_ms={model_load_ms:.3}; records={}",
        inputs.len()
    );

    for (idx, input) in inputs.iter().enumerate() {
        let samples = match read_wav(Path::new(&input.audio_path)) {
            Ok(samples) => samples,
            Err(error) => {
                let row = Output {
                    id: &input.id,
                    transcript: String::new(),
                    status: "error",
                    error: Some(error),
                    run: run_state,
                    model,
                    backend,
                    input_sha256: &input.sha256,
                    duration_s: input.duration_s,
                    load_ms: if idx == 0 { Some(model_load_ms) } else { None },
                    mel_ms: None,
                    encode_ms: None,
                    decode_ms: None,
                    total_ms: None,
                };
                serde_json::to_writer(&mut out, &row)?;
                out.write_all(b"\n")?;
                out.flush()?;
                continue;
            }
        };
        eprintln!(
            "asr_row_boundary phase=record id={} index={} marker=begin",
            input.id, idx
        );
        let started = Instant::now();
        let result = if let Some(engine) = parakeet.as_mut() {
            engine
                .transcribe_final_result_under_lease(&samples)
                .map(|r| r.text)
        } else {
            whisper
                .as_ref()
                .unwrap()
                .transcribe_lang(&samples, &args[5])
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "asr_row_boundary phase=record id={} index={} marker=end outer_call_ms={elapsed_ms:.3}",
            input.id, idx
        );
        let (transcript, status, error) = match result {
            Ok(text) => (text, "success", None),
            Err(e) => (String::new(), "error", Some(e)),
        };
        let row = Output {
            id: &input.id,
            transcript,
            status,
            error,
            run: run_state,
            model,
            backend,
            input_sha256: &input.sha256,
            duration_s: input.duration_s,
            load_ms: if idx == 0 { Some(model_load_ms) } else { None },
            mel_ms: None,
            encode_ms: None,
            decode_ms: None,
            total_ms: Some(elapsed_ms),
        };
        serde_json::to_writer(&mut out, &row)?;
        out.write_all(b"\n")?;
        out.flush()?;
        if (idx + 1) % 5 == 0 {
            eprintln!(
                "progress={}/{} last_total_ms={elapsed_ms:.1}",
                idx + 1,
                inputs.len()
            );
        }
    }
    Ok(())
}
