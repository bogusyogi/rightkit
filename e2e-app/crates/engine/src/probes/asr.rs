//! rightkit-asr: transcribe real speech with the Parakeet TDT ONNX model. The speech is rendered by the OS voice
//! (probes::tts::speech_16k), so the model hears a real voice; the recognised words, their timings, and the hand-off
//! into rightkit-transcript (validated Transcript, SRT) are asserted.
//!
//! Needs, and fails naming them when absent:
//!   * the ONNX Runtime library (see `probes::onnx`);
//!   * a Parakeet TDT ONNX export in `<resources>/models/asr/` (`<resources>` = `Contents/Resources` beside a macOS
//!     `Contents/MacOS` executable, the executable's directory on Windows): `encoder-model.onnx` (+ `.onnx_data` if the
//!     export has one), `decoder_joint-model.onnx`, `vocab.txt` (`ParakeetTDT::from_pretrained`).
//! The CoreML Parakeet and whisper.cpp engines are not exercised here: CoreML needs an `.mlpackage` bundle
//! (`CoreMlParakeet`), whisper.cpp the catalog-verified library staged as a `whisper-cpp` native library plus a ggml model.
use rightkit_asr::{EngineSupport, ExecutionConfig, ExecutionProvider, ParakeetTDT, TimestampMode, Transcriber};
use rightkit_transcript::{build_cues, to_srt, CaptionConfig, EndSource, ModelRef, Newline, TimedWord, Transcript};
use serde_json::{json, Value};

const TEXT: &str = "The quick brown fox jumps over the lazy dog near the river bank.";
const KEYWORDS: [&str; 6] = ["quick", "brown", "fox", "lazy", "dog", "river"];

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let support = EngineSupport::current();
    if !support.parakeet_onnx {
        return Err("this build does not host the Parakeet ONNX engine".into());
    }
    let dir = super::models::asset_dir("asr")?;
    let missing: Vec<&str> =
        ["encoder-model.onnx", "decoder_joint-model.onnx", "vocab.txt"].into_iter().filter(|f| !dir.join(f).is_file()).collect();
    if !missing.is_empty() {
        return Err(format!(
            "needs a Parakeet TDT ONNX export in {} (missing {}: encoder-model.onnx[+.onnx_data], decoder_joint-model.onnx, vocab.txt)",
            dir.display(),
            missing.join(", ")
        ));
    }
    super::onnx::ensure_runtime()?;

    let (audio, tts) = super::tts::speech_16k(TEXT)?;
    let seconds = audio.len() as f64 / 16_000.0;
    let mut model = ParakeetTDT::from_pretrained(&dir, Some(ExecutionConfig::new().with_execution_provider(ExecutionProvider::Cpu)))
        .map_err(|e| format!("ParakeetTDT::from_pretrained: {e}"))?;
    let result = model
        .transcribe_samples(audio, 16_000, 1, Some(TimestampMode::Words))
        .map_err(|e| format!("transcribe_samples: {e}"))?;

    let heard = result.text.to_lowercase();
    let hit: Vec<&str> = KEYWORDS.iter().copied().filter(|k| heard.contains(k)).collect();
    if hit.len() < 5 {
        return Err(format!("the model heard {:?}; only {} of {} keywords ({:?})", result.text, hit.len(), KEYWORDS.len(), hit));
    }
    if result.tokens.len() < 8 {
        return Err(format!("only {} timed words for a twelve-word sentence", result.tokens.len()));
    }
    let mut last = 0f32;
    for t in &result.tokens {
        if t.start < last - 0.001 || t.end < t.start || t.end as f64 > seconds + 0.5 {
            return Err(format!("word {:?} timed {}..{} (audio is {seconds:.2}s, previous end {last})", t.text, t.start, t.end));
        }
        last = t.end;
    }

    // Hand-off: the recognised words become a validated Transcript and SRT captions.
    let words: Vec<TimedWord> = result.tokens.iter().map(|t| TimedWord::from_seconds(t.text.trim(), t.start, t.end)).filter(|w| !w.text.is_empty()).collect();
    let transcript = Transcript::from_words(
        &words,
        "en",
        Some((seconds * 1000.0) as u64),
        ModelRef { id: "parakeet-tdt".into(), revision: String::new(), backend: "onnx-cpu".into() },
        EndSource::Model,
    );
    transcript.validate().map_err(|e| format!("transcript from ASR words: {e}"))?;
    let srt = to_srt(&build_cues(&transcript.timed_words(), &CaptionConfig::default()), Newline::Lf);
    if !srt.contains("-->") {
        return Err("no SRT cue was produced from the recognised words".into());
    }

    Ok(json!({
        "text": result.text,
        "keywords_heard": hit.len(),
        "words": result.tokens.len(),
        "audio_seconds": seconds,
        "tts_backend": tts.backend,
        "engines": { "parakeet_onnx": support.parakeet_onnx, "parakeet_coreml": support.parakeet_coreml, "whisper_cpp": support.whisper_cpp },
    }))
}
