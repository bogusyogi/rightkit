//! E1 — decode-time context-bias SCORE SWEEP (offline, isolated from the app).
//!
//! Question it answers: when a brand IS in the bias list, does raising
//! `RIGHTKIT_ASR_CONTEXT_BIAS_SCORE` actually make the greedy CoreML decoder emit it —
//! and does a benign homophone ("heard right") survive the higher score?
//!
//! It loads the Parakeet TDT bundle ONCE, then for each score re-installs the
//! bias phrases and re-decodes every clip, printing `clip: transcript`. Score 0
//! = bias OFF (raw greedy); those score-0 transcripts are the INPUT to E2
//! (phonetic_recover.py), which tests open-vocab recovery.
//!
//! The bias terms below are a SMALL, in-list set — deliberately NOT a catalog. A
//! big always-on brand list is the wrong call (O(N) per token + wrecks precision
//! on normal speech); the bias list is for the user's personal/contextual set.
//! See docs/plans/2026-06-23-contextual-recognition.md.
//!
//! Run (CPU is fine for an offline experiment and skips the ~4-min ANE compile;
//! the decode RESULT is identical to ANE — only speed differs):
//!   RIGHTKIT_COREML_CU=cpuOnly cargo run --release --example bias_sweep -- \
//!     ../model_registry/coreml/parakeet-tdt-v3  ~/hr-bias-clips

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("bias_sweep is macOS-only (drives CoreML.framework directly).");
}

#[cfg(target_os = "macos")]
const TERMS: &[&str] = &[
    // The test brands (what you record). In-list only — NOT a catalog.
    "HeardRight",
    "Heard Right",
    "OneClickDrive",
    "Zephyr",
    "Instagram",
    "YouTube",
    "TikTok",
    "Nestle",
    "Oracle",
    "Amazon",
    "Netflix",
    "Hershey's",
    "Huawei",
    "Squarespace",
];

#[cfg(target_os = "macos")]
const SCORES: &[f32] = &[0.0, 1.0, 2.5, 3.5, 5.0];

#[cfg(target_os = "macos")]
fn load_wav_16k_mono(path: &std::path::Path) -> Result<Vec<f32>, String> {
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
fn main() {
    use rightkit_dictation_engine::coreml_asr::CoreMlParakeet;
    use std::path::PathBuf;

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: bias_sweep <bundle_dir> <clip.wav | clips_dir> ...");
        std::process::exit(2);
    }
    let bundle = PathBuf::from(&args[1]);

    // Expand inputs: each arg is a .wav or a dir of .wav.
    let mut wavs: Vec<PathBuf> = Vec::new();
    for a in &args[2..] {
        let p = PathBuf::from(a);
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    let q = e.path();
                    if q.extension().map(|x| x == "wav").unwrap_or(false) {
                        wavs.push(q);
                    }
                }
            }
        } else {
            wavs.push(p);
        }
    }
    wavs.sort();
    if wavs.is_empty() {
        eprintln!("no .wav inputs found");
        std::process::exit(2);
    }

    eprintln!(
        "loading {} (RIGHTKIT_COREML_CU={}) — first ANE load can take minutes; cpuOnly loads in seconds",
        bundle.display(),
        std::env::var("RIGHTKIT_COREML_CU").unwrap_or_else(|_| "default(ANE)".into())
    );
    let mut model = CoreMlParakeet::load(&bundle).unwrap_or_else(|e| {
        eprintln!("load failed: {e}");
        std::process::exit(1);
    });

    let clips: Vec<(String, Vec<f32>)> = wavs
        .iter()
        .map(|w| {
            (
                w.file_name().unwrap().to_string_lossy().into_owned(),
                load_wav_16k_mono(w).unwrap_or_default(),
            )
        })
        .collect();

    println!(
        "# {} clips · bias terms ({}): {}",
        clips.len(),
        TERMS.len(),
        TERMS.join(", ")
    );
    for &score in SCORES {
        let label = if score > 0.0 {
            let n = model.set_context_bias_phrases(TERMS.iter(), score);
            format!("score={score:.1} (installed {n}/{})", TERMS.len())
        } else {
            model.clear_context_bias();
            "OFF (raw greedy — feed these into E2)".to_string()
        };
        println!("\n===== bias {label} =====");
        for (name, audio) in &clips {
            if audio.is_empty() {
                println!("  {name}: <wav read error>");
                continue;
            }
            match model.transcribe(audio) {
                Ok(t) => println!("  {name}: {t}"),
                Err(e) => println!("  {name}: <decode error: {e}>"),
            }
        }
    }
}
