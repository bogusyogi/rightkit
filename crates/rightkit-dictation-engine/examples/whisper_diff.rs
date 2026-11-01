//! Run the bundled ANE-native whisper-multi CoreML model on stdin audio.
//! Usage: <python dump_f32.py clip.wav> | cargo run --example whisper_diff -- <whisper-multi-dir>
//! stdin = little-endian f32 mono 16 kHz samples. Prints the transcript.
//! Uses the EXACT app code path (rightkit_dictation_engine::whisper_coreml::WhisperCoreMl).

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("whisper_diff is a macOS-only diagnostic (CoreML/ANE whisper-multi).");
}

#[cfg(target_os = "macos")]
use rightkit_dictation_engine::whisper_coreml::WhisperCoreMl;
#[cfg(target_os = "macos")]
use std::io::Read;
#[cfg(target_os = "macos")]
use std::path::Path;

#[cfg(target_os = "macos")]
fn main() {
    let model_dir = std::env::args()
        .nth(1)
        .expect("usage: whisper_diff <whisper-multi-dir>  (f32le mono 16k on stdin)");
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).expect("read stdin");
    let audio: Vec<f32> = buf
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // Optional 2nd arg = language ("auto" detects, "fr"/"de"/… forces). Default auto.
    let lang = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "auto".to_string());
    let w = WhisperCoreMl::load(Path::new(&model_dir)).expect("load whisper-multi");
    let lang_tok = w.lang_token(&lang);
    let text = w
        .transcribe_lang_windowed(&audio, lang_tok)
        .expect("transcribe");
    println!("{text}");
}
