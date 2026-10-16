//! Deterministic 0.6B Base fingerprint for regression checks across versions:
//! `base_fingerprint <model dir> <voice-clone-prompt.pb>` prints sample count and a hash
//! of the PCM bits for a fixed sentence with greedy decoding (top_k = 1, one thread).
use std::hash::{Hash, Hasher};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let engine = qts::Qwen3TtsEngine::from_model_dir(&args[1]).expect("load model");
    let prompt = engine
        .decode_voice_clone_prompt(&std::fs::read(&args[2]).expect("read prompt"))
        .expect("decode prompt");
    let req = qts::SynthesizeRequest {
        text: "The quick brown fox jumps over the lazy dog.".into(),
        temperature: 1.0,
        top_k: 1,
        thread_count: 1,
        vocoder_thread_count: 1,
        ..Default::default()
    };
    let out = engine
        .synthesize_with_voice_clone_prompt(&req, &prompt)
        .expect("synthesize");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in &out.pcm_f32 {
        s.to_bits().hash(&mut h);
    }
    println!(
        "samples={} rate={} hash={:016x}",
        out.pcm_f32.len(),
        out.sample_rate_hz,
        h.finish()
    );
}
