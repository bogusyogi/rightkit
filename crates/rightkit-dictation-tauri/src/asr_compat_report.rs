//! The user-facing result of the ASR compatibility check, over [`crate::asr_compat`].
//!
//! [`crate::asr_compat`] measures; this module turns a measurement into the serialisable result a
//! settings screen shows (a status code, the numbers and a message) and runs the whole check:
//! list the captures, pick the sample, read the current profile, transcribe, judge. The host
//! passes the captures folder, the thresholds, the profile names, the transcribe call and the
//! message templates ([`CompatLabels`]).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::asr_compat::{
    evaluate, pick_sample, wav_duration_secs, wavs_newest_first, SamplePick, Thresholds, Verdict,
};

/// The result the renderer reads. `status` is one of `needs_sample`, `needs_longer_sample`,
/// `degraded`, `compatible`, `inconclusive`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CompatReport {
    pub status: &'static str,
    pub duration_secs: f64,
    pub word_count: usize,
    pub words_per_second: f64,
    pub max_no_emission_gap_secs: Option<f64>,
    pub recommended_profile: Option<&'static str>,
    pub message: String,
}

/// Message templates. `{words}` is the word count and `{secs}` the duration in seconds (no
/// decimals, except `compatible` which shows one). Replace every field with the host's copy.
#[derive(Debug, Clone)]
pub struct CompatLabels {
    /// No capture exists.
    pub needs_sample: String,
    /// The newest capture is too short (`{secs}`).
    pub needs_longer_sample: String,
    /// Degraded and a different profile is recommended (`{words}`, `{secs}`).
    pub degraded_retry: String,
    /// Degraded on the fallback profile already (`{words}`, `{secs}`).
    pub degraded_final: String,
    /// Compatible (`{words}`, `{secs}`).
    pub compatible: String,
    /// Inconclusive.
    pub inconclusive: String,
}

impl Default for CompatLabels {
    fn default() -> Self {
        Self {
            needs_sample: "No captured recording found. Record a longer sample of continuous speech, then rerun.".into(),
            needs_longer_sample: "Latest capture is {secs}s. Record a longer sample of continuous speech, then rerun.".into(),
            degraded_retry: "The current route decoded {words} words across {secs}s. Switch the compute profile, record again, then rerun this test.".into(),
            degraded_final: "The fallback profile decoded only {words} words across {secs}s. Export diagnostics for review.".into(),
            compatible: "The current route passed this sample: {words} words across {secs}s without a long emission gap.".into(),
            inconclusive: "Inconclusive. Record a longer sample of continuous speech with few pauses, then rerun.".into(),
        }
    }
}

fn fill(template: &str, words: usize, secs: String) -> String {
    template
        .replace("{words}", &words.to_string())
        .replace("{secs}", &secs)
}

/// Judge one transcribed sample and word it. `text` is the transcript text (words are counted by
/// whitespace), `words` its `(start_ms, end_ms)` pairs; the other inputs are [`evaluate`]'s.
pub fn report_for(
    duration_secs: f64,
    text: &str,
    words: impl IntoIterator<Item = (u64, u64)>,
    current_profile: &str,
    fallback_profile: &'static str,
    thresholds: &Thresholds,
    labels: &CompatLabels,
) -> CompatReport {
    let evaluation = evaluate(
        duration_secs,
        text.split_whitespace().count(),
        words,
        current_profile,
        fallback_profile,
        thresholds,
    );
    let (word_count, duration_secs) = (evaluation.word_count, evaluation.duration_secs);
    let (status, message) = match evaluation.verdict {
        Verdict::Degraded => (
            "degraded",
            if evaluation.recommended_profile.is_some() {
                fill(
                    &labels.degraded_retry,
                    word_count,
                    format!("{duration_secs:.0}"),
                )
            } else {
                fill(
                    &labels.degraded_final,
                    word_count,
                    format!("{duration_secs:.0}"),
                )
            },
        ),
        Verdict::Compatible => (
            "compatible",
            fill(
                &labels.compatible,
                word_count,
                format!("{duration_secs:.1}"),
            ),
        ),
        Verdict::Inconclusive => ("inconclusive", labels.inconclusive.clone()),
    };
    CompatReport {
        status,
        duration_secs,
        word_count,
        words_per_second: evaluation.words_per_second,
        max_no_emission_gap_secs: evaluation.max_gap_secs,
        recommended_profile: evaluation.recommended_profile,
        message,
    }
}

/// The transcript a host's `transcribe` closure returns: its text and word times in milliseconds.
pub struct SampleTranscript {
    pub text: String,
    pub words: Vec<(u64, u64)>,
}

/// Run the whole check over the WAV captures in `audio_dir`. Steps, in order: list the captures
/// (a missing folder is an error), pick the newest long enough, read the current profile with
/// `current_profile`, run `transcribe` on the picked file, judge. A missing or too-short capture
/// is a normal result (`needs_sample`, `needs_longer_sample`), not an error. The host calls this on
/// a blocking thread and logs the start and the outcome.
pub fn run_sample_check(
    audio_dir: &Path,
    thresholds: &Thresholds,
    fallback_profile: &'static str,
    labels: &CompatLabels,
    current_profile: impl FnOnce() -> String,
    transcribe: impl FnOnce(PathBuf) -> Result<SampleTranscript, String>,
) -> Result<CompatReport, String> {
    let captures = wavs_newest_first(audio_dir)?;
    let (path, duration_secs) =
        match pick_sample(&captures, thresholds.min_sample_secs, wav_duration_secs)? {
            SamplePick::NoCapture => {
                return Ok(CompatReport {
                    status: "needs_sample",
                    duration_secs: 0.0,
                    word_count: 0,
                    words_per_second: 0.0,
                    max_no_emission_gap_secs: None,
                    recommended_profile: None,
                    message: labels.needs_sample.clone(),
                });
            }
            SamplePick::TooShort { newest_secs } => {
                return Ok(CompatReport {
                    status: "needs_longer_sample",
                    duration_secs: newest_secs,
                    word_count: 0,
                    words_per_second: 0.0,
                    max_no_emission_gap_secs: None,
                    recommended_profile: None,
                    message: fill(&labels.needs_longer_sample, 0, format!("{newest_secs:.0}")),
                });
            }
            SamplePick::Found {
                path,
                duration_secs,
            } => (path, duration_secs),
        };
    let current = current_profile();
    let transcript = transcribe(path)?;
    Ok(report_for(
        duration_secs,
        &transcript.text,
        transcript.words,
        &current,
        fallback_profile,
        thresholds,
        labels,
    ))
}
