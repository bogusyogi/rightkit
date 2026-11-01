//! rightkit-vad: the span policy (hysteresis, padding, chunking) on scripted scores, then the real Silero model over
//! OS-voice speech (probes::tts::speech_16k) padded with digital silence: speech spans must sit on the speech, none in
//! the silence, and the streaming path (odd callback sizes) must reproduce the batch scores.
//!
//! Needs, and fails naming them when absent:
//!   * the ONNX Runtime library (see `probes::onnx`);
//!   * the Silero model `silero_vad_16k_op15.onnx` (MIT, about 2 MB) at `<resources>/models/vad/silero_vad_16k_op15.onnx`
//!     (`<resources>` = `Contents/Resources` beside a macOS `Contents/MacOS` executable, the executable's directory on Windows).
use rightkit_vad::{
    chunk_spans, detect, scores_to_spans, ChunkConfig, FrameScorer, FrameStream, Silero, SileroOptions, Span, SpeechScorer, VadConfig,
    FRAME_MS, FRAME_SAMPLES,
};
use serde_json::{json, Value};

fn policy_checks() -> Result<(), String> {
    // 100 frames of 32 ms: speech at frames 10..40 and 60..70, with a 3-frame dip inside the first.
    let mut scores = vec![0.05f32; 100];
    for s in &mut scores[10..40] {
        *s = 0.9;
    }
    for s in &mut scores[24..27] {
        *s = 0.2; // below neg_threshold but shorter than min_silence (120 ms = 4 frames): must not split
    }
    for s in &mut scores[60..70] {
        *s = 0.8;
    }
    let cfg = VadConfig::default();
    let spans = scores_to_spans(&scores, &cfg, 100 * FRAME_MS);
    if spans.len() != 2 {
        return Err(format!("expected 2 spans from the scripted scores, got {spans:?}"));
    }
    let (a, b) = (spans[0], spans[1]);
    if a.start_ms != 10 * FRAME_MS - cfg.pad_ms || a.end_ms != 40 * FRAME_MS + cfg.pad_ms {
        return Err(format!("first span {a:?} is not padded speech 10..40"));
    }
    if b.start_ms != 60 * FRAME_MS - cfg.pad_ms {
        return Err(format!("second span {b:?}"));
    }
    // A speech blip shorter than min_speech is dropped.
    let mut blip = vec![0.05f32; 40];
    blip[10] = 0.9;
    if !scores_to_spans(&blip, &cfg, 40 * FRAME_MS).is_empty() {
        return Err("a one-frame blip became a span".into());
    }
    // Chunking: near spans merge, an over-long span splits at the quietest frame, short chunks drop.
    let chunks = chunk_spans(
        &[Span { start_ms: 0, end_ms: 1_000 }, Span { start_ms: 1_100, end_ms: 2_000 }, Span { start_ms: 10_000, end_ms: 10_050 }],
        &ChunkConfig::default(),
        None,
    );
    if chunks != [Span { start_ms: 0, end_ms: 2_000 }] {
        return Err(format!("chunk_spans gave {chunks:?}"));
    }
    let long = chunk_spans(&[Span { start_ms: 0, end_ms: 70_000 }], &ChunkConfig { max_chunk_ms: 30_000, ..ChunkConfig::default() }, None);
    if long.len() != 3 || long.iter().any(|c| c.len_ms() > 30_000) {
        return Err(format!("a 70 s span did not split into three chunks of at most 30 s: {long:?}"));
    }
    // Bad thresholds are refused.
    let bad = VadConfig { threshold: 0.3, neg_threshold: 0.6, ..VadConfig::default() };
    if SpeechScorer::Energy.detect(&[0.0; 1024], &bad).is_ok() {
        return Err("neg_threshold above threshold was accepted".into());
    }
    Ok(())
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    policy_checks()?;

    let model = super::models::asset_dir("vad")?.join("silero_vad_16k_op15.onnx");
    if !model.is_file() {
        return Err(format!(
            "needs the Silero VAD model at {} (silero_vad_16k_op15.onnx); the span-policy checks passed",
            model.display()
        ));
    }
    super::onnx::ensure_runtime()?;

    let (speech, tts) = super::tts::speech_16k("The quick brown fox jumps over the lazy dog. Then it rests by the river.")?;
    let (lead_ms, tail_ms) = (2_000usize, 2_000usize);
    let speech_ms = speech.len() as u64 / 16;
    let mut audio = vec![0.0f32; lead_ms * 16];
    audio.extend_from_slice(&speech);
    audio.extend(vec![0.0f32; tail_ms * 16]);

    let (mut scorer, report) = SpeechScorer::from_path(&model, SileroOptions::default()).map_err(|e| format!("load Silero: {e}"))?;
    if report.fallback_reason.is_some() {
        return Err(format!("Silero fell back: {:?}", report.fallback_reason));
    }
    let spans = scorer.detect(&audio, &VadConfig::default()).map_err(|e| format!("detect: {e}"))?;
    let (first, last) = (spans.first().ok_or("no speech span found in speech")?, spans.last().ok_or("no spans")?);
    if first.start_ms + 200 < lead_ms as u64 {
        return Err(format!("leading silence marked as speech: {first:?}"));
    }
    if last.end_ms > lead_ms as u64 + speech_ms + 300 {
        return Err(format!("trailing silence marked as speech: {last:?}"));
    }
    let covered: u64 = spans.iter().map(Span::len_ms).sum();
    if covered * 100 < speech_ms * 50 {
        return Err(format!("speech under-covered: {covered} ms of {speech_ms} ms"));
    }
    // Digital silence yields no speech.
    let quiet = scorer.detect(&vec![0.0f32; 16_000 * 3], &VadConfig::default()).map_err(|e| format!("detect silence: {e}"))?;
    if !quiet.is_empty() {
        return Err(format!("silence produced spans {quiet:?}"));
    }
    // Non-finite input is refused.
    let mut poisoned = vec![0.0f32; 2048];
    poisoned[7] = f32::NAN;
    if scorer.detect(&poisoned, &VadConfig::default()).is_ok() {
        return Err("NaN samples were accepted".into());
    }

    // Streaming through awkward callback sizes reproduces the batch scores.
    let batch = scorer.scores(&audio).map_err(|e| format!("scores: {e}"))?;
    let mut stream = FrameStream::new(Silero::load(&model).map_err(|e| format!("load streaming Silero: {e}"))?, 0.5);
    let mut streamed = Vec::new();
    let mut at = 0usize;
    for size in [777usize, 160, 1, 4096, 513].iter().cycle() {
        if at >= audio.len() {
            break;
        }
        let end = (at + size).min(audio.len());
        streamed.extend(stream.push(&audio[at..end]).ok_or("the stream failed")?.scores);
        at = end;
    }
    let whole_frames = audio.len() / FRAME_SAMPLES;
    if streamed.len() != whole_frames || batch.len() < whole_frames {
        return Err(format!("streamed {} frames, expected {whole_frames} (batch {})", streamed.len(), batch.len()));
    }
    let worst = streamed.iter().zip(&batch).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    if worst > 1e-3 {
        return Err(format!("streaming scores differ from batch by {worst}"));
    }
    let mut frame_scorer = Silero::load(&model).map_err(|e| format!("load: {e}"))?;
    frame_scorer.reset();
    if detect(&mut frame_scorer, &audio, &VadConfig::default()).map_err(|e| format!("detect via FrameScorer: {e}"))?.is_empty() {
        return Err("detect over a bare Silero found no speech".into());
    }

    Ok(json!({
        "backend": report.backend.name(),
        "spans": spans.iter().map(|s| [s.start_ms, s.end_ms]).collect::<Vec<_>>(),
        "speech_ms": speech_ms,
        "covered_ms": covered,
        "frames": streamed.len(),
        "stream_vs_batch_max_diff": worst,
        "tts_backend": tts.backend,
    }))
}
