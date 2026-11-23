//! The ASR compatibility check over a captured recording.
//!
//! A user who keeps raw audio captures can ask whether the current engine route decodes a long,
//! continuous sample sensibly. This module owns the mechanics: listing the captured WAV files, reading
//! a WAV's duration, picking the newest sample that is long enough, and turning a transcript's word
//! count and word times into a verdict (degraded, compatible, inconclusive) with the longest gap
//! without an emitted word. The host owns where the captures live, every threshold
//! ([`Thresholds`]), the profile names and every word the user reads.

use std::path::{Path, PathBuf};

/// The numbers that decide a verdict. All supplied by the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// A capture shorter than this cannot be judged (seconds).
    pub min_sample_secs: f64,
    /// Fewer words per second than this is degraded.
    pub degraded_words_per_second: f64,
    /// A gap without an emitted word of at least this is degraded (seconds).
    pub degraded_gap_secs: f64,
    /// The last word ending before this percent of the duration is an early stop, degraded.
    pub early_stop_percent: i64,
    /// A sample that is not degraded is compatible only with at least this many words per second ...
    pub compatible_words_per_second: f64,
    /// ... and a longest gap of at most this (seconds).
    pub compatible_gap_secs: f64,
}

/// The verdict on one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Degraded,
    Compatible,
    Inconclusive,
}

/// What [`evaluate`] measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub duration_secs: f64,
    pub word_count: usize,
    pub words_per_second: f64,
    /// Longest stretch (seconds) with no word, including the tail after the last word; `None` when
    /// the transcript has no timed word.
    pub max_gap_secs: Option<f64>,
    /// The profile to try next: set only for a degraded sample, and only when it is not the
    /// profile already in use.
    pub recommended_profile: Option<&'static str>,
}

/// Which capture to judge.
#[derive(Debug, Clone, PartialEq)]
pub enum SamplePick {
    /// There is no capture.
    NoCapture,
    /// No capture is long enough; `newest_secs` is the newest one's duration.
    TooShort { newest_secs: f64 },
    /// The newest capture that is long enough.
    Found { path: PathBuf, duration_secs: f64 },
}

/// The `.wav` files directly inside `dir`, newest modification time first. A missing or unreadable
/// folder is an error; an entry with no readable time sorts last.
pub fn wavs_newest_first(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut captures = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|error| format!("read captured audio: {error}"))? {
        let entry = entry.map_err(|error| format!("read captured audio entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("wav") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        captures.push((modified, path));
    }
    captures.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    Ok(captures.into_iter().map(|(_, path)| path).collect())
}

/// Duration in seconds of a RIFF/WAVE file's bytes: the `data` chunk length over the `fmt ` byte
/// rate. Chunk bounds are checked; a missing or zero byte rate or a missing `data` chunk is an error.
pub fn wav_duration_secs_from_bytes(bytes: &[u8]) -> Result<f64, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("captured audio is not a valid WAV file".to_string());
    }
    let mut byte_rate = None;
    let mut data_len = None;
    let mut offset = 12usize;
    while offset.saturating_add(8) <= bytes.len() {
        let chunk_len = u32::from_le_bytes(
            bytes[offset + 4..offset + 8]
                .try_into()
                .map_err(|_| "invalid WAV chunk length")?,
        ) as usize;
        let body = offset + 8;
        let end = body
            .checked_add(chunk_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "invalid WAV chunk bounds".to_string())?;
        match &bytes[offset..offset + 4] {
            b"fmt " if chunk_len >= 12 => {
                byte_rate = Some(u32::from_le_bytes(
                    bytes[body + 8..body + 12]
                        .try_into()
                        .map_err(|_| "invalid WAV byte rate")?,
                ));
            }
            b"data" => data_len = Some(chunk_len as u64),
            _ => {}
        }
        offset = end.saturating_add(chunk_len % 2);
    }
    let byte_rate = byte_rate
        .filter(|value| *value > 0)
        .ok_or_else(|| "captured WAV is missing valid format metadata".to_string())?;
    let data_len = data_len.ok_or_else(|| "captured WAV is missing audio data".to_string())?;
    Ok(data_len as f64 / byte_rate as f64)
}

/// [`wav_duration_secs_from_bytes`] over a file.
pub fn wav_duration_secs(path: &Path) -> Result<f64, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read captured WAV: {error}"))?;
    wav_duration_secs_from_bytes(&bytes)
}

/// Choose the sample to judge from `captures` (newest first): the first one of at least
/// `min_sample_secs`. `duration` reads one capture's length; every capture is read until one
/// qualifies, and the newest is always read.
pub fn pick_sample(
    captures: &[PathBuf],
    min_sample_secs: f64,
    duration: impl Fn(&Path) -> Result<f64, String>,
) -> Result<SamplePick, String> {
    let Some(newest) = captures.first() else {
        return Ok(SamplePick::NoCapture);
    };
    let newest_secs = duration(newest)?;
    for path in captures {
        let duration_secs = duration(path)?;
        if duration_secs >= min_sample_secs {
            return Ok(SamplePick::Found {
                path: path.clone(),
                duration_secs,
            });
        }
    }
    Ok(SamplePick::TooShort { newest_secs })
}

/// Judge one sample. `words` are the transcript's `(start_ms, end_ms)` pairs; `word_count` is its
/// whitespace-separated word count. `current_profile` is the profile in use and `fallback_profile`
/// the one to recommend for a degraded sample.
pub fn evaluate(
    duration_secs: f64,
    word_count: usize,
    words: impl IntoIterator<Item = (u64, u64)>,
    current_profile: &str,
    fallback_profile: &'static str,
    thresholds: &Thresholds,
) -> Evaluation {
    let words_per_second = word_count as f64 / duration_secs.max(0.001);
    let duration_ms = (duration_secs * 1000.0).round() as i64;
    let mut max_gap_ms = None;
    let mut last_end_ms = None;
    for (start, end) in words {
        // Word times arrive as u64 ms; this arithmetic stays i64 (identical values for every real
        // timestamp).
        let (start_ms, end_ms) = (start as i64, end as i64);
        let previous_end = last_end_ms.unwrap_or(0);
        let gap = start_ms.saturating_sub(previous_end).max(0);
        max_gap_ms = Some(max_gap_ms.unwrap_or(0).max(gap));
        last_end_ms = Some(last_end_ms.unwrap_or(0).max(end_ms));
    }
    if let Some(last_end) = last_end_ms {
        let trailing_gap = duration_ms.saturating_sub(last_end).max(0);
        max_gap_ms = Some(max_gap_ms.unwrap_or(0).max(trailing_gap));
    }
    let max_gap_secs = max_gap_ms.map(|value| value as f64 / 1000.0);
    let early_stop =
        last_end_ms.is_some_and(|last| last < duration_ms * thresholds.early_stop_percent / 100);
    let degraded = word_count == 0
        || words_per_second < thresholds.degraded_words_per_second
        || max_gap_secs.is_some_and(|gap| gap >= thresholds.degraded_gap_secs)
        || early_stop;
    let compatible = !degraded
        && words_per_second >= thresholds.compatible_words_per_second
        && max_gap_secs.is_none_or(|gap| gap <= thresholds.compatible_gap_secs);
    let recommended_profile = degraded
        .then_some(fallback_profile)
        .filter(|profile| *profile != current_profile);
    let verdict = if degraded {
        Verdict::Degraded
    } else if compatible {
        Verdict::Compatible
    } else {
        Verdict::Inconclusive
    };
    Evaluation {
        verdict,
        duration_secs,
        word_count,
        words_per_second,
        max_gap_secs,
        recommended_profile,
    }
}
