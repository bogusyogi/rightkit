//! Room tone: capture a sample of the room's noise floor from the quietest stationary
//! non-speech region, then build a graph fragment that fills cut gaps (and J/L edges)
//! with it so the edit never drops to digital silence.
//!
//! New logic; the old pipeline only measured the floor step across a join
//! (`audio_finish::measure_room_tone_step`, ported in [`crate::measure`]).

use serde::{Deserialize, Serialize};

use crate::error::AudioError;
use crate::filter::{num, Filter};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomToneParams {
    /// Analysis frame length.
    pub frame_ms: u32,
    /// Preferred minimum sample length.
    pub min_len_ms: u32,
    /// Accepted when no region reaches `min_len_ms`.
    pub fallback_min_len_ms: u32,
    /// Longest sample captured.
    pub max_len_ms: u32,
    /// Max dB spread between the loudest and quietest frame in the window (stationarity).
    pub max_spread_db: f64,
    /// Regions louder than this (dBFS RMS) are never room tone.
    pub max_level_db: f64,
    /// Keep this far from any speech span.
    pub speech_margin_ms: i64,
    /// Without VAD spans, frames above floor + this many dB count as speech.
    pub envelope_speech_db: f64,
}

impl Default for RoomToneParams {
    fn default() -> Self {
        Self {
            frame_ms: 20,
            min_len_ms: 400,
            fallback_min_len_ms: 250,
            max_len_ms: 2000,
            max_spread_db: 6.0,
            max_level_db: -35.0,
            speech_margin_ms: 80,
            envelope_speech_db: 12.0,
        }
    }
}

impl RoomToneParams {
    pub fn validate(&self) -> Result<(), AudioError> {
        let bad = self.frame_ms == 0
            || self.fallback_min_len_ms < self.frame_ms * 2
            || self.min_len_ms < self.fallback_min_len_ms
            || self.max_len_ms < self.min_len_ms
            || !self.max_spread_db.is_finite()
            || !self.max_level_db.is_finite()
            || self.speech_margin_ms < 0;
        if bad {
            Err(AudioError::Invalid("room tone params inconsistent".into()))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RoomToneSample {
    pub start_ms: i64,
    pub duration_ms: i64,
    pub rms_db: f64,
}

/// A span of the output timeline (ms) that needs room tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapSpan {
    pub start_ms: i64,
    pub end_ms: i64,
}

fn power_db(power: f64) -> f64 {
    10.0 * (power + 1e-12).log10()
}

/// Find the quietest stationary non-speech window in mono `samples`.
/// `speech_spans_ms` are VAD/transcript speech spans (source-time ms matching `samples`);
/// pass an empty slice to fall back to an envelope test.
pub fn find_room_tone(
    samples: &[f32],
    sample_rate: u32,
    speech_spans_ms: &[(i64, i64)],
    params: &RoomToneParams,
) -> Result<RoomToneSample, AudioError> {
    params.validate()?;
    let frame = (sample_rate as usize * params.frame_ms as usize / 1000).max(1);
    let powers: Vec<f64> = samples
        .chunks_exact(frame)
        .map(|c| c.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / c.len() as f64)
        .collect();
    if powers.is_empty() {
        return Err(AudioError::NoRoomTone("audio is too short".into()));
    }
    let db: Vec<f64> = powers.iter().map(|p| power_db(*p)).collect();
    let n = db.len();
    let ms_per_frame = f64::from(params.frame_ms);
    let margin = (params.speech_margin_ms as f64 / ms_per_frame).ceil() as usize;

    let mut speech = vec![false; n];
    if speech_spans_ms.is_empty() {
        let mut sorted = db.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let floor = sorted[n / 10];
        let flagged: Vec<usize> = (0..n)
            .filter(|i| db[*i] > floor + params.envelope_speech_db)
            .collect();
        for i in flagged {
            for flag in speech
                .iter_mut()
                .take((i + margin + 1).min(n))
                .skip(i.saturating_sub(margin))
            {
                *flag = true;
            }
        }
    } else {
        for (s, e) in speech_spans_ms {
            let a = ((*s as f64 / ms_per_frame).floor() as i64 - margin as i64).max(0) as usize;
            let b = (((*e as f64 / ms_per_frame).ceil() as i64) + margin as i64).max(0) as usize;
            for flag in speech.iter_mut().take(b.min(n)).skip(a.min(n)) {
                *flag = true;
            }
        }
    }

    for min_ms in [params.min_len_ms, params.fallback_min_len_ms] {
        let min_frames = (f64::from(min_ms) / ms_per_frame).ceil() as usize;
        let max_frames = (f64::from(params.max_len_ms) / ms_per_frame).floor() as usize;
        let mut best: Option<(f64, usize, usize)> = None; // (mean_db, start, len)
        let mut i = 0;
        while i < n {
            if speech[i] {
                i += 1;
                continue;
            }
            let run_start = i;
            while i < n && !speech[i] {
                i += 1;
            }
            let run_len = i - run_start;
            if run_len < min_frames {
                continue;
            }
            let w = run_len.min(max_frames.max(min_frames));
            for start in run_start..=(run_start + run_len - w) {
                let win = &db[start..start + w];
                let (lo, hi) = win
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
                if hi - lo > params.max_spread_db {
                    continue;
                }
                let mean = power_db(powers[start..start + w].iter().sum::<f64>() / w as f64);
                if mean <= params.max_level_db && best.is_none_or(|(m, _, _)| mean < m) {
                    best = Some((mean, start, w));
                }
            }
        }
        if let Some((mean, start, w)) = best {
            return Ok(RoomToneSample {
                start_ms: (start as f64 * ms_per_frame) as i64,
                duration_ms: (w as f64 * ms_per_frame) as i64,
                rms_db: mean,
            });
        }
    }
    Err(AudioError::NoRoomTone(format!(
        "no stationary window <= {} dBFS of at least {} ms outside speech",
        params.max_level_db, params.fallback_min_len_ms
    )))
}

/// Extend each gap by `overlap_ms`, merge overlaps, drop empties.
pub fn expand_gaps(gaps: &[GapSpan], overlap_ms: i64, total_ms: Option<i64>) -> Vec<GapSpan> {
    let mut v: Vec<GapSpan> = gaps
        .iter()
        .filter(|g| g.end_ms > g.start_ms)
        .map(|g| GapSpan {
            start_ms: (g.start_ms - overlap_ms).max(0),
            end_ms: total_ms.map_or(g.end_ms + overlap_ms, |t| (g.end_ms + overlap_ms).min(t)),
        })
        .collect();
    v.sort_by_key(|g| g.start_ms);
    let mut out: Vec<GapSpan> = Vec::new();
    for g in v {
        match out.last_mut() {
            Some(l) if g.start_ms <= l.end_ms => l.end_ms = l.end_ms.max(g.end_ms),
            _ => out.push(g),
        }
    }
    out
}

/// Graph fragment that turns a (conditioned) room-tone sample on `tone_in` into a bed of
/// room tone placed only under `gaps`, silent elsewhere, `total_ms` long, label `out`.
///
/// The sample is looped forward-then-reversed so the loop seam is sample-continuous, each
/// gap reads a different offset of the loop (no audible repetition), and each fill fades
/// in/out over `overlap_ms` so the tone slides under the neighbouring speech.
/// `prefix` namespaces internal labels so several fragments can coexist in one graph.
#[allow(clippy::too_many_arguments)]
pub fn room_tone_fill_fragment(
    tone_in: &str,
    tone_duration_ms: i64,
    sample_rate: u32,
    gaps: &[GapSpan],
    gain_db: f64,
    overlap_ms: i64,
    total_ms: i64,
    prefix: &str,
    out: &str,
) -> Result<String, AudioError> {
    if tone_duration_ms < 100 {
        return Err(AudioError::Invalid("room tone sample under 100 ms".into()));
    }
    if overlap_ms < 0 {
        return Err(AudioError::Invalid("room tone overlap must be >= 0".into()));
    }
    let gaps = expand_gaps(gaps, overlap_ms, Some(total_ms));
    if gaps.is_empty() {
        return Err(AudioError::Invalid("no gaps to fill with room tone".into()));
    }
    let n = gaps.len();
    let unit_ms = tone_duration_ms * 2;
    let unit_samples = i64::from(sample_rate) * unit_ms / 1000;
    let secs = |ms: i64| num(ms as f64 / 1000.0);
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "[{tone_in}]aformat=sample_rates={sample_rate}:channel_layouts=mono,asplit=2[{prefix}_a][{prefix}_b]"
    ));
    parts.push(format!("[{prefix}_b]areverse[{prefix}_r]"));
    parts.push(format!(
        "[{prefix}_a][{prefix}_r]concat=n=2:v=0:a=1[{prefix}_unit]"
    ));
    let loops: String = (0..n).map(|i| format!("[{prefix}_l{i}]")).collect();
    parts.push(format!(
        "[{prefix}_unit]aloop=loop=-1:size={unit_samples}:start=0,asplit={n}{loops}"
    ));
    for (i, g) in gaps.iter().enumerate() {
        let dur = g.end_ms - g.start_ms;
        let fade = overlap_ms.min(dur / 2).max(1);
        let off = (i as i64 * 733) % unit_ms;
        let f = Filter::new("volume").opt("volume", format!("{}dB", num(gain_db)));
        parts.push(format!(
            "[{prefix}_l{i}]atrim=start={}:end={},asetpts=PTS-STARTPTS,afade=t=in:d={},afade=t=out:st={}:d={},{},adelay=delays={}:all=1[{prefix}_g{i}]",
            secs(off),
            secs(off + dur),
            secs(fade),
            secs(dur - fade),
            secs(fade),
            f.render(),
            g.start_ms,
        ));
    }
    let ins: String = (0..n).map(|i| format!("[{prefix}_g{i}]")).collect();
    parts.push(format!(
        "{ins}amix=inputs={n}:normalize=0:duration=longest,apad=whole_dur={},atrim=end={}[{out}]",
        secs(total_ms),
        secs(total_ms)
    ));
    Ok(parts.join(";"))
}
