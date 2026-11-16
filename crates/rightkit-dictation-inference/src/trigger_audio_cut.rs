use crate::asr;
use rightkit_text_polish::text_pipeline::{
    strip_fired_control_tail, strip_kws_onset_cut_residue, ControlIntent,
};

const SAMPLE_RATE: usize = 16_000;
const COMMAND_PREROLL_SAMPLES: usize = SAMPLE_RATE / 4;

/// Experiment switches for the cut resolver and the control cleanup (lever
/// matrix section F). `Default` equals the shipped constants and behaviour;
/// production callers pass `CutResolverSwitches::default()`. A harness may
/// build a non-default value to pull one lever at a time.
///
/// Route applicability: `seam_search` and `preroll_samples` act only on the
/// Static15-seam route (`resolve_trigger_audio_cut`); `cleanup_first_alignment`
/// and `straddle_tolerance_sec` act only on the TDT cleanup-first route
/// (`control_cleanup`); `residue_strip` acts on both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutResolverSwitches {
    /// F1: false skips the Static15 seam search, so the cut is the KWS onset.
    pub seam_search: bool,
    /// F2/F3: sample count entering the VAD plausibility bound
    /// `onset + preroll + 1/32 s < floor` (NOT a cut offset). Default 250 ms.
    pub preroll_samples: usize,
    /// F4 (NOT "all cleanup disabled"): false bypasses only the named strip
    /// functions. On the cleanup-first route `cleaned_text` still runs boundary
    /// trim (`drop_words_from_boundary`) and `recover_rendering`, which may still
    /// remove text. False skips (`strip_kws_onset_cut_residue`
    /// and `strip_fired_control_tail`) on the Static15 route, and the
    /// `strip_fired_control_tail` text strips on the cleanup-first route.
    pub residue_strip: bool,
    /// F5: false skips the forced-alignment arm of cleanup; the result is the
    /// text strip only (method `alignment_disabled`).
    pub cleanup_first_alignment: bool,
    /// F6/F7: how far before the aligned boundary a straddling word may start
    /// and still pull the cut back to its start. Default 0.16 s.
    pub straddle_tolerance_sec: f32,
}

impl Default for CutResolverSwitches {
    fn default() -> Self {
        Self {
            seam_search: true,
            preroll_samples: COMMAND_PREROLL_SAMPLES,
            residue_strip: true,
            cleanup_first_alignment: true,
            straddle_tolerance_sec: crate::control_cleanup::STRADDLE_START_TOLERANCE_SEC,
        }
    }
}

/// Text finalisation of a decoded confirmed-control utterance: the KWS-onset
/// residue strip (only when the cut stood at the onset), then the fired
/// control-tail strip. With default switches this is exactly the previous
/// inline logic in `recording_finish.rs`.
pub fn strip_control_residue(
    text: &str,
    kws_onset_cut: bool,
    intent: ControlIntent,
    switches: &CutResolverSwitches,
) -> String {
    if !switches.residue_strip {
        return text.to_string();
    }
    let residue_stripped = if kws_onset_cut {
        strip_kws_onset_cut_residue(text)
    } else {
        text.to_string()
    };
    strip_fired_control_tail(&residue_stripped, intent)
}

/// `cut_source` of a cut made at the KWS onset because no pause preceded it.
pub const KWS_ONSET_CUT_SOURCE: &str = "kws_onset";

#[derive(Debug, PartialEq)]
pub enum TriggerAudioCut {
    Cut {
        onset_sample: usize,
        cut_at: usize,
        cut_source: &'static str,
        seam_rms: Option<f32>,
        noise_floor_rms: Option<f32>,
    },
    FullBuffer {
        reason: &'static str,
    },
}

/// Resolves a safe low-energy cut before a confirmed control utterance.
pub fn resolve_trigger_audio_cut(
    trigger_onset_sample: Option<usize>,
    onset_skip_reason: Option<&'static str>,
    speech_start_sample: Option<usize>,
    samples: &[f32],
) -> TriggerAudioCut {
    resolve_trigger_audio_cut_with_switches(
        trigger_onset_sample,
        onset_skip_reason,
        speech_start_sample,
        samples,
        &CutResolverSwitches::default(),
    )
}

/// `resolve_trigger_audio_cut` with explicit experiment switches.
pub fn resolve_trigger_audio_cut_with_switches(
    trigger_onset_sample: Option<usize>,
    onset_skip_reason: Option<&'static str>,
    speech_start_sample: Option<usize>,
    samples: &[f32],
    switches: &CutResolverSwitches,
) -> TriggerAudioCut {
    let Some(onset) = trigger_onset_sample else {
        return TriggerAudioCut::FullBuffer {
            reason: onset_skip_reason.unwrap_or("no_timestamps"),
        };
    };
    let floor = speech_start_sample.unwrap_or(0);
    let max_vad_preroll = switches.preroll_samples.saturating_add(SAMPLE_RATE / 32);
    if onset == 0 || onset > samples.len() || onset.saturating_add(max_vad_preroll) < floor {
        return TriggerAudioCut::FullBuffer {
            reason: "implausible",
        };
    }
    let seam = if switches.seam_search {
        asr::confident_static15_terminal_cut(samples, floor, onset)
    } else {
        None
    };
    match seam {
        Some(seam) if seam.cut_at > floor => TriggerAudioCut::Cut {
            onset_sample: onset,
            cut_at: seam.cut_at,
            cut_source: "static15_seam",
            seam_rms: Some(seam.seam_rms),
            noise_floor_rms: Some(seam.noise_floor_rms),
        },
        // No pause before the command: cut exactly at the spotter's onset.
        // The spotter fires slightly late, so the caller must strip the
        // "zephyr" fragment with `strip_kws_onset_cut_residue` (measured on
        // 103 no-pause recordings, 2026-09-23).
        _ if onset > floor => TriggerAudioCut::Cut {
            onset_sample: onset,
            cut_at: onset,
            cut_source: KWS_ONSET_CUT_SOURCE,
            seam_rms: None,
            noise_floor_rms: None,
        },
        _ => TriggerAudioCut::FullBuffer {
            reason: "no_precommand_speech",
        },
    }
}

/// Drops every whole word that STARTS at or after exact `boundary_sec`
/// from the final text. The boundary comes from a forced alignment of the
/// spotter-confirmed command, so no spelling is consulted. Trims trailing words
/// of `text` by count and only when they agree with the token words; at most
/// eight words. Non-finite boundaries or token timestamps are rejected.
/// `None` means "leave the text alone".
pub fn drop_words_from_boundary(
    text: &str,
    tokens: &[crate::TimedToken],
    boundary_sec: f32,
) -> Option<(String, Vec<crate::TimedToken>)> {
    if !boundary_sec.is_finite() {
        return None;
    }
    let mut words: Vec<(usize, f32, String)> = Vec::new();
    let mut previous_start = None;
    for (index, token) in tokens.iter().enumerate() {
        if !token.start.is_finite()
            || !token.end.is_finite()
            || token.start < 0.0
            || token.end < 0.0
            || token.end < token.start
            || previous_start.is_some_and(|start| token.start < start)
        {
            return None;
        }
        previous_start = Some(token.start);
        if token.text.starts_with(char::is_whitespace) || words.is_empty() {
            words.push((index, token.start, token.text.trim().to_string()));
        } else if let Some(word) = words.last_mut() {
            word.2.push_str(&token.text);
        }
    }
    let boundary = words.iter().position(|word| word.1 >= boundary_sec)?;
    if words[boundary..].iter().any(|word| word.1 < boundary_sec) {
        return None;
    }
    let dropped = words.len() - boundary;
    if dropped == 0 || dropped > 8 {
        return None;
    }
    let plain = |word: &str| {
        word.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let text_words: Vec<&str> = text.split_whitespace().collect();
    if text_words.len() < dropped
        || text_words[text_words.len() - dropped..]
            .iter()
            .zip(&words[boundary..])
            .any(|(a, b)| plain(a) != plain(&b.2))
    {
        return None;
    }
    let mut end = text.trim_end().len();
    for _ in 0..dropped {
        let head = text[..end].trim_end();
        end = head.rfind(char::is_whitespace).unwrap_or(0);
    }
    Some((
        text[..end].trim_end().to_string(),
        tokens[..words[boundary].0].to_vec(),
    ))
}
