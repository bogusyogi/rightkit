//! Replays a saved recording through the production final decode (conditioning
//! authority + the same `AsrRuntime::transcribe_result`) and prints the final
//! text, the probe-sized prefixes the live lane decoded, and a few padded
//! variants. Used to reproduce "probes had words, final came back blank".
//!
//! Usage: blank_final_replay <models-root> <wav> [<wav>...]

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use std::path::PathBuf;
    const SR: usize = 16_000;
    let mut args = std::env::args_os().skip(1);
    let models = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: blank_final_replay <models-root> <wav>...")?;
    std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    let mut asr = AsrRuntime::load(&models, AsrEp::resolve_default())?;
    for wav in args.map(PathBuf::from) {
        let raw: Vec<f32> = rightkit_audio::wav::read_wav_mono_f32(&wav)
            .map_err(|e| e.to_string())?
            .samples;
        let condition = |pcm: &[f32]| {
            rightkit_dictation_core::audio_conditioning::condition_for_asr(
                pcm, SR as u32, "default",
            )
        };
        // Exactly the production calls: the final goes through
        // `transcribe_final_result_under_lease`, a live preview through
        // `transcribe_under_lease`, each after `apply_utterance_bias`.
        let mut decode = |label: &str, audio: &[f32]| {
            let final_text = asr
                .with_inference_lease("final_asr", |m| {
                    rightkit_dictation_engine::asr::apply_utterance_bias(m);
                    m.transcribe_final_result_under_lease(audio)
                })
                .map(|r| r.text)
                .unwrap_or_else(|e| format!("<err {e}>"));
            let preview_text = asr
                .with_inference_lease("opening_command_asr", |m| {
                    rightkit_dictation_engine::asr::apply_utterance_bias(m);
                    m.transcribe_under_lease(audio)
                })
                .unwrap_or_else(|e| format!("<err {e}>"));
            println!(
                "  {label:<34} {:>6} samples  final={:?}  preview={:?}",
                audio.len(),
                final_text,
                preview_text
            );
        };
        println!(
            "{} ({} samples, {:.2}s)",
            wav.display(),
            raw.len(),
            raw.len() as f32 / SR as f32
        );
        let full = condition(&raw);
        decode("FINAL (production)", &full);
        decode("FINAL again", &full);
        for cut in [
            raw.len() * 80 / 100,
            raw.len() * 90 / 100,
            raw.len() * 95 / 100,
        ] {
            decode(
                &format!("prefix {:.2}s", cut as f32 / SR as f32),
                &condition(&raw[..cut]),
            );
        }
        decode("raw, unconditioned", &raw);
        if let Some(spans) = std::env::var_os("RIGHTKIT_REPLAY_SPANS") {
            for span in spans.to_string_lossy().split(',') {
                let Some((a, b)) = span.split_once(':') else {
                    continue;
                };
                let (a, b): (usize, usize) = (
                    a.parse().unwrap_or(0),
                    b.parse::<usize>().unwrap_or(raw.len()).min(raw.len()),
                );
                decode(&format!("span {a}..{b}"), &condition(&raw[a..b]));
            }
        }
        for skip_ms in [100_usize, 200, 300, 500] {
            decode(
                &format!("conditioned, first {skip_ms} ms dropped"),
                &condition(&raw[skip_ms * SR / 1000..]),
            );
        }
        for pad_ms in [250_usize, 500, 1000] {
            let mut tail = full.clone();
            tail.extend(std::iter::repeat_n(0.0, pad_ms * SR / 1000));
            decode(&format!("final + {pad_ms} ms trailing silence"), &tail);
            let mut lead = vec![0.0; pad_ms * SR / 1000];
            lead.extend_from_slice(&full);
            decode(&format!("final + {pad_ms} ms leading silence"), &lead);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("blank_final_replay is macOS-only");
}
