// Padded-window decode contract (docs/ASR_DECODE_CONTRACT.md): quiet-cut
// boundary search, seam de-duplication, and the shared timing constants every
// platform's windowed decode must agree on.
pub(crate) const SAMPLE_RATE: usize = 16_000;
pub(crate) const PADDED_WINDOW_MS: u64 = 15_000;
pub(crate) const PADDED_WINDOW_PADDING_MS: u64 = 2_240;
pub(crate) const PADDED_WINDOW_SILENCE_MS: u64 = 200;
pub(crate) const PADDED_WINDOW_SILENCE_HOP_MS: u64 = 100;
pub(crate) const PADDED_WINDOW_OVERLAP_CHARS: usize = 16;

/// Return Static15's quietest 200 ms / 100 ms-hop cut inside a bounded range.
pub(crate) fn quietest_static15_cut_in(samples: &[f32], lo: usize, hi: usize) -> Option<usize> {
    quietest_static15_cut_scored_in(samples, lo, hi).map(|(cut, _)| cut)
}

fn quietest_static15_cut_scored_in(samples: &[f32], lo: usize, hi: usize) -> Option<(usize, f32)> {
    let win = ms_to_samples(PADDED_WINDOW_SILENCE_MS);
    let hop = ms_to_samples(PADDED_WINDOW_SILENCE_HOP_MS);
    if hi <= lo || hi.saturating_sub(lo) < win {
        return None;
    }
    let mut best = None;
    let mut i = lo;
    while i + win <= hi {
        let value = rms(&samples[i..i + win]);
        if best.map(|(_, best)| value < best).unwrap_or(true) {
            best = Some((i + win / 2, value));
        }
        i += hop;
    }
    best
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Static15TerminalCut {
    pub cut_at: usize,
    pub seam_rms: f32,
    pub noise_floor_rms: f32,
}

/// Find a terminal-command seam no more than 800 ms before the KWS cascade's emitted
/// first-keyword timestamp. RMS minima are ambiguous during continuous speech,
/// so accept only a 200 ms candidate that belongs to the recording's quietest
/// decile of like-sized pre-trigger windows. Low-confidence audio stays full-buffer.
pub fn confident_static15_terminal_cut(
    samples: &[f32],
    floor: usize,
    end: usize,
) -> Option<Static15TerminalCut> {
    const MAX_ROLLBACK_MS: u64 = 800;
    let hi = end.min(samples.len());
    let half_window = ms_to_samples(PADDED_WINDOW_SILENCE_MS) / 2;
    let lo = hi
        .saturating_sub(ms_to_samples(MAX_ROLLBACK_MS).saturating_add(half_window))
        .max(floor.min(hi));
    let (cut_at, seam_rms) = quietest_static15_cut_scored_in(samples, lo, hi)?;
    if cut_at > hi || hi.saturating_sub(cut_at) > ms_to_samples(MAX_ROLLBACK_MS) {
        return None;
    }

    let window = ms_to_samples(PADDED_WINDOW_SILENCE_MS);
    let hop = ms_to_samples(PADDED_WINDOW_SILENCE_HOP_MS);
    let mut frame_rms = Vec::new();
    let mut cursor = floor.min(hi);
    while cursor.saturating_add(window) <= hi {
        let value = rms(&samples[cursor..cursor + window]);
        if value.is_finite() {
            frame_rms.push(value);
        }
        cursor = cursor.saturating_add(hop);
    }
    if frame_rms.is_empty() {
        return None;
    }
    frame_rms.sort_by(f32::total_cmp);
    let noise_floor_rms = frame_rms[(frame_rms.len() - 1) / 10];
    let median_rms = frame_rms[frame_rms.len() / 2];
    (seam_rms <= noise_floor_rms && noise_floor_rms < median_rms * 0.8).then_some(
        Static15TerminalCut {
            cut_at,
            seam_rms,
            noise_floor_rms,
        },
    )
}

pub(crate) fn quiet_cut_in(samples: &[f32], lo: usize, hi: usize) -> Option<usize> {
    quietest_static15_cut_in(samples, lo, hi)
}

/// Text-only padded-window decode, for backends whose decode returns a string
/// with no token timestamps (Whisper).
///
/// Same contract as `transcribe_padded_window` — see `docs/ASR_DECODE_CONTRACT.md`
/// — with the token bookkeeping dropped: decode `[start, cut)`, commit whatever
/// comes back, join seams with the shared overlap rule. `window_samples` is the
/// BACKEND's native window (15 s Parakeet, 30 s Whisper); the trailing search
/// region stays `PADDED_WINDOW_PADDING_MS` for both, since 2.24 s is already
/// ample to contain an inter-word pause and a larger region would waste more of
/// each window.
// Only the CoreML Whisper lane and the padded-window tests call this; the
// Whisper lane is macOS-only, so on other targets it is dead.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn transcribe_padded_window_text<F>(
    samples: &[f32],
    window_samples: usize,
    mut decode: F,
) -> Result<String, String>
where
    F: FnMut(&[f32]) -> Result<String, String>,
{
    if samples.len() <= window_samples {
        return decode(samples);
    }

    let padding = ms_to_samples(PADDED_WINDOW_PADDING_MS);
    let target = window_samples.saturating_sub(padding);
    let mut text = String::new();
    let mut start = 0usize;

    while start < samples.len() {
        let window_end = (start + window_samples).min(samples.len());
        if window_end >= samples.len() {
            break;
        }
        let target_end = (start + target).min(samples.len());
        let cut = quiet_cut_in(samples, target_end, window_end).unwrap_or(window_end);
        let segment: &[f32] = if start < cut {
            &samples[start..cut]
        } else {
            &[]
        };
        append_with_overlap(&mut text, &decode(segment)?);
        start = cut.max(start + 1);
    }
    if start < samples.len() {
        append_with_overlap(&mut text, &decode(&samples[start..])?);
    }
    Ok(text.trim().to_string())
}

/// Join the next window's text onto `buffer`, de-duplicating the seam.
///
/// The `overlap == 0` branch matters: with disjoint windows (decode exactly to
/// the quiet cut) there is never a character overlap to consume, and a raw
/// `push_str` welds the last word of one window to the first of the next —
/// "AndersonCounty", "Star Trekhas". Each weld costs a deletion AND a
/// substitution, which measured as +1.24 WER points on the canonical corpus.
/// Swift's `appendWithOverlap` always had this separator; Rust did not.
pub(crate) fn append_with_overlap(buffer: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    if buffer.is_empty() {
        buffer.push_str(next);
        return;
    }
    let overlap = suffix_prefix_overlap(buffer, next, PADDED_WINDOW_OVERLAP_CHARS);
    let remainder = &next[overlap..];
    if remainder.is_empty() {
        return;
    }
    // Only insert a separator when the seam would otherwise fuse two words.
    let needs_space = overlap == 0
        && !buffer.ends_with(char::is_whitespace)
        && !remainder.starts_with(char::is_whitespace);
    if needs_space {
        buffer.push(' ');
    }
    buffer.push_str(remainder);
}

pub(crate) fn suffix_prefix_overlap(a: &str, b: &str, max_chars: usize) -> usize {
    let max = a.len().min(b.len()).min(max_chars);
    let mut best = 0usize;
    for len in 1..=max {
        if !a.is_char_boundary(a.len() - len) || !b.is_char_boundary(len) {
            continue;
        }
        // A character match inside an unrelated word can weld the seam:
        // "that" + " Turtles" used to consume " T" and render
        // "thaturtles". Only consume a complete prefix at word edges.
        let ends_inside_word = b[..len].chars().last().is_some_and(char::is_alphanumeric)
            && b[len..].chars().next().is_some_and(char::is_alphanumeric);
        if ends_inside_word && len < 3 {
            continue;
        }
        if a[a.len() - len..].eq_ignore_ascii_case(&b[..len]) {
            best = len;
        }
    }
    best
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

pub(crate) fn padded_window_samples() -> usize {
    ms_to_samples(PADDED_WINDOW_MS)
}

pub(crate) fn ms_to_samples(ms: u64) -> usize {
    (SAMPLE_RATE as u64)
        .saturating_mul(ms)
        .saturating_div(1_000) as usize
}
