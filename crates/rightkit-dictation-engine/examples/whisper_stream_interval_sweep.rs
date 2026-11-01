//! Diagnostic only: sweep Whisper Core ML background-commit INTERVAL against
//! decode CONTEXT, to measure the residual unprocessed tail at key release and
//! what it costs in WER and encoder compute.
//!
//! Nothing here changes a production default. Every arm is selected by CLI flag.
//!
//! Usage:
//!   whisper_stream_interval_sweep <whisper-multi-dir> <eval_set.json dir> <out_dir> [arm ...]
//!
//! Arms:
//!   deferred30            production post-Stop path (`transcribe_lang_windowed`)
//!   checkpoint:<N>        disjoint quiet-cut segments of N seconds during hold
//!   rolling:<I>:<C>       every I s decode the trailing C s, word-overlap merge
//!   agree:<I>:<C>         same rolling decode, but LocalAgreement-2: commit only
//!                         the prefix two consecutive hypotheses agree on
//!
//! Whisper's encoder always consumes a zero-padded 30 s mel, so an encoder pass
//! costs the same whether it is handed 3 s or 30 s of real audio. `context` is
//! therefore free up to 30 s and `interval` is the only compute knob.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod imp {
    use rightkit_dictation_engine::whisper_coreml::WhisperCoreMl;
    use serde::Deserialize;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    pub const SR: usize = 16_000;
    pub const WHISPER_WINDOW: usize = 30 * SR;
    // Contract constants (docs/ASR_DECODE_CONTRACT.md §2).
    pub const PADDING_MS: usize = 2_240;
    pub const SILENCE_MS: usize = 200;
    pub const HOP_MS: usize = 100;
    pub const OVERLAP_CHARS: usize = 16;
    // Harness-local: word-level overlap allowance for rolling arms, where the
    // re-decoded left context is seconds long rather than a seam of a few chars.
    pub const MAX_WORD_OVERLAP: usize = 400;

    #[derive(Deserialize)]
    pub struct Item {
        pub wav: String,
        pub gt: String,
        #[allow(dead_code)]
        pub corpus: String,
        pub dur_s: f64,
    }

    #[derive(Clone, Copy, Debug)]
    pub enum Arm {
        Deferred30,
        /// `pad_ms` is the trailing region the quiet cut searches. `None` uses
        /// the proportional rule `min(2240ms, window/4)`, which SHRINKS the
        /// search region below 9 s windows and therefore confounds short-window
        /// arms with a second changed variable. Pass it explicitly to hold the
        /// production 2240 ms and isolate the interval.
        Checkpoint {
            window_s: f64,
            pad_ms: Option<usize>,
        },
        Rolling {
            interval_s: f64,
            context_s: f64,
        },
        /// LocalAgreement-2 (Macháček, Dabre & Bojar): decode a rolling buffer,
        /// but commit ONLY the prefix on which two consecutive hypotheses agree.
        /// The unstable remainder is held and re-decided next fire. This is the
        /// arm the plain `rolling` arms lack — they commit every hypothesis in
        /// full, which is why a re-decode that changes its mind corrupts text
        /// that was already emitted.
        Agree {
            interval_s: f64,
            context_s: f64,
        },
    }

    impl Arm {
        pub fn parse(spec: &str) -> Result<Arm, String> {
            let parts: Vec<&str> = spec.split(':').collect();
            match parts.as_slice() {
                ["deferred30"] => Ok(Arm::Deferred30),
                ["checkpoint", n] => Ok(Arm::Checkpoint {
                    window_s: n.parse().map_err(|_| format!("bad window {n}"))?,
                    pad_ms: None,
                }),
                ["checkpoint", n, p] => Ok(Arm::Checkpoint {
                    window_s: n.parse().map_err(|_| format!("bad window {n}"))?,
                    pad_ms: Some(p.parse().map_err(|_| format!("bad pad {p}"))?),
                }),
                ["agree", i, c] => Ok(Arm::Agree {
                    interval_s: i.parse().map_err(|_| format!("bad interval: {i}"))?,
                    context_s: c.parse().map_err(|_| format!("bad context: {c}"))?,
                }),
                ["rolling", i, c] => Ok(Arm::Rolling {
                    interval_s: i.parse().map_err(|_| format!("bad interval {i}"))?,
                    context_s: c.parse().map_err(|_| format!("bad context {c}"))?,
                }),
                _ => Err(format!("unknown arm {spec}")),
            }
        }
        pub fn label(&self) -> String {
            match self {
                Arm::Deferred30 => "deferred30".into(),
                Arm::Checkpoint { window_s, pad_ms } => match pad_ms {
                    Some(p) => format!("checkpoint-{window_s}-pad{p}"),
                    None => format!("checkpoint-{window_s}"),
                },
                Arm::Rolling {
                    interval_s,
                    context_s,
                } => format!("rolling-{interval_s}-{context_s}"),
                Arm::Agree {
                    interval_s,
                    context_s,
                } => format!("agree-{interval_s}-{context_s}"),
            }
        }
    }

    pub struct ClipRun {
        pub text: String,
        /// Wall ms spent in background decodes that happen WHILE the key is held.
        pub background_ms: u128,
        /// Wall ms spent after key release (the user-visible Stop latency).
        pub tail_ms: u128,
        /// Seconds of audio still unprocessed at key release.
        pub tail_s: f64,
        /// Full 30 s encoder passes during hold.
        pub bg_passes: usize,
        /// Full 30 s encoder passes after release.
        pub tail_passes: usize,
    }

    pub fn load_wav(path: &Path) -> Vec<f32> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        wav.samples
    }

    fn rms(x: &[f32]) -> f32 {
        rightkit_dictation_core::audio_conditioning::rms(x)
    }

    /// Contract rule 3, parameterised by the search region so short windows can
    /// use a proportionally smaller trailing region.
    pub fn quiet_cut(search: &[f32]) -> Option<usize> {
        let span = SILENCE_MS * SR / 1000;
        let hop = HOP_MS * SR / 1000;
        if search.len() < span {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        let mut off = 0usize;
        while off + span <= search.len() {
            let level = rms(&search[off..off + span]);
            if best.map(|(_, b)| level < b).unwrap_or(true) {
                best = Some((off + span / 2, level));
            }
            off += hop;
        }
        best.map(|(i, _)| i)
    }

    /// Contract seam join: de-duplicate up to 16 chars, else insert a separator.
    pub fn append_with_overlap(buffer: &mut String, next: &str) {
        let next = next.trim();
        if next.is_empty() {
            return;
        }
        if buffer.is_empty() {
            buffer.push_str(next);
            return;
        }
        let b = buffer.to_lowercase();
        let n = next.to_lowercase();
        let max = OVERLAP_CHARS.min(b.len()).min(n.len());
        for k in (1..=max).rev() {
            if b.is_char_boundary(b.len() - k)
                && n.is_char_boundary(k)
                && b[b.len() - k..] == n[..k]
            {
                buffer.push_str(&next[k..]);
                return;
            }
        }
        buffer.push(' ');
        buffer.push_str(next);
    }

    fn norm_word(w: &str) -> String {
        w.to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    /// Harness-local word-level merge for rolling arms: the new hypothesis
    /// re-decodes seconds of already-committed audio, so the overlap is many
    /// words, not the 16-character seam the disjoint contract uses — and the
    /// re-decode is NOT word-identical to what was committed, so an exact
    /// suffix/prefix match misses and duplicates the whole window (measured at
    /// 310% WER before this was replaced).
    ///
    /// Instead, align: find the prefix length `k` of `next` that is closest, by
    /// word edit distance, to the committed tail, and append only `next[k..]`.
    /// This is the alignment step Whisper-Streaming needs and gets from token
    /// timestamps; this Whisper decode returns no times, so it is recovered
    /// from the text.
    pub fn merge_words(committed: &mut Vec<String>, next: &[String]) {
        if next.is_empty() {
            return;
        }
        if committed.is_empty() {
            committed.extend_from_slice(next);
            return;
        }
        let tail_len = MAX_WORD_OVERLAP.min(committed.len());
        let tail: Vec<String> = committed[committed.len() - tail_len..]
            .iter()
            .map(|w| norm_word(w))
            .collect();
        let nn: Vec<String> = next.iter().map(|w| norm_word(w)).collect();

        // Full Levenshtein DP; the last row gives edit distance from `tail` to
        // every prefix of `next`.
        let mut prev: Vec<usize> = (0..=nn.len()).collect();
        for (i, tw) in tail.iter().enumerate() {
            let mut cur = vec![i + 1; nn.len() + 1];
            for (j, nw) in nn.iter().enumerate() {
                cur[j + 1] = (prev[j + 1] + 1)
                    .min(cur[j] + 1)
                    .min(prev[j] + usize::from(tw != nw));
            }
            prev = cur;
        }
        // Prefer the LONGEST prefix among ties: a longer consumed prefix means
        // less duplicated text, and ties are common on silence-padded ends.
        let mut best_k = 0usize;
        let mut best = usize::MAX;
        for (k, &d) in prev.iter().enumerate() {
            if d <= best {
                best = d;
                best_k = k;
            }
        }
        committed.extend_from_slice(&next[best_k..]);
    }

    /// The alignment half of `merge_words`: how many leading words of `next`
    /// re-state audio already committed. Split out so LocalAgreement can align
    /// first and then decide, per word, whether to COMMIT or hold.
    pub fn aligned_prefix(committed: &[String], next: &[String]) -> usize {
        if next.is_empty() || committed.is_empty() {
            return 0;
        }
        let tail_len = MAX_WORD_OVERLAP.min(committed.len());
        let tail: Vec<String> = committed[committed.len() - tail_len..]
            .iter()
            .map(|w| norm_word(w))
            .collect();
        let nn: Vec<String> = next.iter().map(|w| norm_word(w)).collect();
        let mut prev: Vec<usize> = (0..=nn.len()).collect();
        for (i, tw) in tail.iter().enumerate() {
            let mut cur = vec![i + 1; nn.len() + 1];
            for (j, nw) in nn.iter().enumerate() {
                cur[j + 1] = (prev[j + 1] + 1)
                    .min(cur[j] + 1)
                    .min(prev[j] + usize::from(tw != nw));
            }
            prev = cur;
        }
        let mut best_k = 0usize;
        let mut best = usize::MAX;
        for (k, &d) in prev.iter().enumerate() {
            if d <= best {
                best = d;
                best_k = k;
            }
        }
        best_k
    }

    /// Longest common prefix of two word sequences, compared normalised.
    fn common_prefix(a: &[String], b: &[String]) -> usize {
        a.iter()
            .zip(b.iter())
            .take_while(|(x, y)| norm_word(x) == norm_word(y))
            .count()
    }

    fn decode_seg(model: &WhisperCoreMl, seg: &[f32], lang: i32) -> Result<String, String> {
        let seg = &seg[..seg.len().min(WHISPER_WINDOW)];
        model.transcribe_lang(seg, lang)
    }

    /// Number of full 30 s encoder passes the shipped padded-window walk makes.
    fn deferred_passes(audio: &[f32]) -> usize {
        if audio.len() <= WHISPER_WINDOW {
            return 1;
        }
        let padding = PADDING_MS * SR / 1000;
        let target = WHISPER_WINDOW - padding;
        let mut start = 0usize;
        let mut n = 0usize;
        while start + WHISPER_WINDOW < audio.len() {
            let cut = start
                + target
                + quiet_cut(&audio[start + target..start + WHISPER_WINDOW]).unwrap_or(padding);
            n += 1;
            start = cut.max(start + 1);
        }
        if start < audio.len() {
            n += 1;
        }
        n
    }

    pub fn run_clip(
        model: &WhisperCoreMl,
        audio: &[f32],
        arm: Arm,
        lang: i32,
    ) -> Result<ClipRun, String> {
        match arm {
            Arm::Deferred30 => {
                let t0 = Instant::now();
                let text = model.transcribe_lang_windowed(audio, lang)?;
                Ok(ClipRun {
                    text: text.trim().to_string(),
                    background_ms: 0,
                    tail_ms: t0.elapsed().as_millis(),
                    tail_s: audio.len() as f64 / SR as f64,
                    bg_passes: 0,
                    tail_passes: deferred_passes(audio),
                })
            }
            Arm::Checkpoint { window_s, pad_ms } => {
                let window = (window_s * SR as f64) as usize;
                // Contract padding for windows that can afford it; a proportional
                // quarter-window search region for the short ones (2 240 ms of a
                // 3 s window would be three quarters of the window).
                let padding = match pad_ms {
                    // Explicit region: hold it fixed across windows so the only
                    // thing varying between arms is the interval itself.
                    Some(ms) => (ms * SR / 1000).min(window.saturating_sub(SR / 4)),
                    None => ((PADDING_MS * SR / 1000).min(window / 4)).max(SILENCE_MS * SR / 1000),
                }
                .max(SILENCE_MS * SR / 1000);
                let target = window.saturating_sub(padding);
                let mut text = String::new();
                let mut start = 0usize;
                let mut bg_ms = 0u128;
                let mut passes = 0usize;
                while start + window < audio.len() {
                    let t0 = Instant::now();
                    let cut = start
                        + target
                        + quiet_cut(&audio[start + target..start + window]).unwrap_or(padding);
                    let cut = cut.max(start + 1).min(audio.len());
                    append_with_overlap(&mut text, &decode_seg(model, &audio[start..cut], lang)?);
                    bg_ms += t0.elapsed().as_millis();
                    passes += 1;
                    start = cut;
                }
                let tail_s = (audio.len() - start) as f64 / SR as f64;
                let t0 = Instant::now();
                append_with_overlap(&mut text, &decode_seg(model, &audio[start..], lang)?);
                Ok(ClipRun {
                    text: text.trim().to_string(),
                    background_ms: bg_ms,
                    tail_ms: t0.elapsed().as_millis(),
                    tail_s,
                    bg_passes: passes,
                    tail_passes: 1,
                })
            }
            Arm::Rolling {
                interval_s,
                context_s,
            } => {
                let interval = (interval_s * SR as f64) as usize;
                let context = ((context_s * SR as f64) as usize).min(WHISPER_WINDOW);
                let mut words: Vec<String> = Vec::new();
                let mut committed_to = 0usize; // audio consumed by committed text
                let mut bg_ms = 0u128;
                let mut passes = 0usize;
                let mut next_fire = interval;
                while next_fire < audio.len() {
                    let t0 = Instant::now();
                    let lo = next_fire.saturating_sub(context);
                    let hyp = decode_seg(model, &audio[lo..next_fire], lang)?;
                    let new: Vec<String> = hyp.split_whitespace().map(|s| s.to_string()).collect();
                    merge_words(&mut words, &new);
                    bg_ms += t0.elapsed().as_millis();
                    passes += 1;
                    committed_to = next_fire;
                    next_fire += interval;
                }
                let tail_s = (audio.len() - committed_to) as f64 / SR as f64;
                let t0 = Instant::now();
                let lo = audio.len().saturating_sub(context);
                let hyp = decode_seg(model, &audio[lo..], lang)?;
                let new: Vec<String> = hyp.split_whitespace().map(|s| s.to_string()).collect();
                merge_words(&mut words, &new);
                Ok(ClipRun {
                    text: words.join(" ").trim().to_string(),
                    background_ms: bg_ms,
                    tail_ms: t0.elapsed().as_millis(),
                    tail_s,
                    bg_passes: passes,
                    tail_passes: 1,
                })
            }
            Arm::Agree {
                interval_s,
                context_s,
            } => {
                let interval = (interval_s * SR as f64) as usize;
                let context = ((context_s * SR as f64) as usize).min(WHISPER_WINDOW);
                let mut committed: Vec<String> = Vec::new();
                // The hypothesis tail that has NOT yet been confirmed by a
                // second, independent decode.
                let mut unstable: Vec<String> = Vec::new();
                // Audio behind the last COMMITTED word — not merely the last
                // decode. That is the whole point: residual tail is measured
                // against stable text, so a fire that agrees on nothing
                // correctly counts as no progress.
                let mut committed_to = 0usize;
                let mut bg_ms = 0u128;
                let mut passes = 0usize;
                let mut next_fire = interval;
                while next_fire < audio.len() {
                    let t0 = Instant::now();
                    let lo = next_fire.saturating_sub(context);
                    let hyp = decode_seg(model, &audio[lo..next_fire], lang)?;
                    let new: Vec<String> = hyp.split_whitespace().map(|s| s.to_string()).collect();
                    // Drop the part of this hypothesis that re-states committed
                    // audio, then keep only what agrees with LAST time.
                    let k = aligned_prefix(&committed, &new);
                    let candidate = &new[k..];
                    let agreed = common_prefix(candidate, &unstable);
                    if agreed > 0 {
                        committed.extend_from_slice(&candidate[..agreed]);
                        committed_to = next_fire;
                    }
                    unstable = candidate[agreed..].to_vec();
                    bg_ms += t0.elapsed().as_millis();
                    passes += 1;
                    next_fire += interval;
                }
                let tail_s = (audio.len() - committed_to) as f64 / SR as f64;
                let t0 = Instant::now();
                let lo = audio.len().saturating_sub(context);
                let hyp = decode_seg(model, &audio[lo..], lang)?;
                let new: Vec<String> = hyp.split_whitespace().map(|s| s.to_string()).collect();
                // At stop there is no next hypothesis to agree with, so the
                // final decode is committed whole.
                merge_words(&mut committed, &new);
                Ok(ClipRun {
                    text: committed.join(" ").trim().to_string(),
                    background_ms: bg_ms,
                    tail_ms: t0.elapsed().as_millis(),
                    tail_s,
                    bg_passes: passes,
                    tail_passes: 1,
                })
            }
        }
    }

    pub fn main() {
        let a: Vec<String> = std::env::args().collect();
        if a.len() < 5 {
            eprintln!(
                "usage: whisper_stream_interval_sweep <whisper-dir> <eval-dir> <out-dir> <arm> [arm ...]"
            );
            std::process::exit(2);
        }
        let arms: Vec<Arm> = a[4..]
            .iter()
            .map(|s| Arm::parse(s).unwrap_or_else(|e| panic!("{e}")))
            .collect();

        eprintln!("loading whisper-multi …");
        let model = WhisperCoreMl::load(Path::new(&a[1])).unwrap_or_else(|e| {
            eprintln!("load failed: {e}");
            std::process::exit(1);
        });
        let lang = model.lang_token("en");
        let eval_dir = PathBuf::from(&a[2]);
        let out_dir = PathBuf::from(&a[3]);
        std::fs::create_dir_all(&out_dir).unwrap();
        let items: Vec<Item> =
            serde_json::from_str(&std::fs::read_to_string(eval_dir.join("eval_set.json")).unwrap())
                .unwrap();

        // Warm the ANE / compile caches before any timed work.
        if let Some(first) = items.first() {
            let audio = load_wav(&eval_dir.join(&first.wav));
            let _ = model.transcribe_lang(&audio[..audio.len().min(WHISPER_WINDOW)], lang);
            let _ = model.transcribe_lang(&audio[..audio.len().min(3 * SR)], lang);
        }

        for arm in &arms {
            let label = arm.label();
            eprintln!("=== arm {label} ===");
            let mut rows = Vec::new();
            let started = Instant::now();
            for (i, item) in items.iter().enumerate() {
                let audio = load_wav(&eval_dir.join(&item.wav));
                let clip_id = Path::new(&item.wav)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                match run_clip(&model, &audio, *arm, lang) {
                    Ok(r) => rows.push(serde_json::json!({
                        "clip_id": clip_id,
                        "hyp": r.text,
                        "dur_s": item.dur_s,
                        "gt": item.gt,
                        "elapsed_s": (r.background_ms + r.tail_ms) as f64 / 1000.0,
                        "background_ms": r.background_ms,
                        "tail_ms": r.tail_ms,
                        "tail_s": r.tail_s,
                        "bg_passes": r.bg_passes,
                        "tail_passes": r.tail_passes,
                    })),
                    Err(e) => rows.push(serde_json::json!({
                        "clip_id": clip_id, "error": e, "dur_s": item.dur_s,
                    })),
                }
                if i % 25 == 0 {
                    eprintln!(
                        "  {label}: {}/{} ({:.0}s)",
                        i,
                        items.len(),
                        started.elapsed().as_secs_f64()
                    );
                }
            }
            let out = out_dir.join(format!("{label}.json"));
            std::fs::write(&out, serde_json::to_string(&rows).unwrap()).unwrap();
            eprintln!(
                "arm {label} done in {:.1}s -> {}",
                started.elapsed().as_secs_f64(),
                out.display()
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    imp::main();
}
