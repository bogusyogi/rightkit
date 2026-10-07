//! Real-model smoke test for 0.2 modes:
//! `mode_synth <model dir> <voicedesign|customvoice> <instruct or -> <speaker or -> <out.wav> [text]`
//! Writes 16-bit mono PCM WAV at the model's sample rate and prints samples/duration.
use qts::{ModeSynthesizeRequest, PresetSpeaker, QtsMode, SynthesizeRequest};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = match a[2].as_str() {
        "voicedesign" => QtsMode::VoiceDesign,
        "customvoice" => QtsMode::CustomVoice,
        other => panic!("unknown mode {other}"),
    };
    let instruct = (a[3] != "-").then(|| a[3].clone());
    let speaker = (a[4] != "-").then(|| {
        PresetSpeaker::ALL
            .into_iter()
            .find(|s| format!("{s:?}").eq_ignore_ascii_case(&a[4]))
            .expect("unknown speaker")
    });
    let text = a.get(6).cloned().unwrap_or_else(|| "Hello! This voice was designed from a short text description.".into());
    let engine = qts::Qwen3TtsEngine::from_model_dir_for_mode(&a[1], mode).expect("load model");
    let req = ModeSynthesizeRequest {
        synthesis: SynthesizeRequest { text, temperature: 0.9, top_k: 50, ..Default::default() },
        mode,
        instruct,
        speaker,
        voice_clone_prompt: None,
        speaker_embedding: None,
    };
    let out = engine.synthesize_mode(&req).expect("synthesize");
    let rate = out.sample_rate_hz;
    let mut wav = Vec::with_capacity(44 + out.pcm_f32.len() * 2);
    let data_len = (out.pcm_f32.len() * 2) as u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in &out.pcm_f32 {
        wav.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(&a[5], wav).expect("write wav");
    println!("samples={} seconds={:.2}", out.pcm_f32.len(), out.pcm_f32.len() as f32 / rate as f32);
}
