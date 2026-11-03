//! Production QA check `speech.retranscription`.
//!
//! The delivered render is transcribed again by a host-supplied [`Retranscriber`] (typically the
//! same in-process engine that made the source transcript). Its words are compared with the words
//! the edit expects the render to say:
//!
//! - `missing`: expected words the render does not say (a dropped word);
//! - `extra`: words the render says that the edit removed (a survivor) or never
//!   expected.
//!
//! The check passes when `missing + extra <= max_word_errors`. The default is 0, so
//! one dropped or surviving word fails it. An unavailable speech model prints `SKIP:` and
//! records a failed check; a skip never passes.
//!
//! The check adds to a [`QaReport`] with [`QaReport::with_check`]. [`qa_gate_with_retranscription`]
//! records that report and its evidence in the project receipt.

use std::path::Path;

use serde_json::{json, Value};

use crate::render_qa::error::QaError;
use crate::render_qa::probe::QaTools;
use crate::render_qa::receipt::{record_report, QaOutcome};
use crate::render_qa::report::{CheckStatus, QaCheck, QaReport, Severity};
use crate::render_qa::run::run_qa;
use crate::render_qa::spec::{QaExpect, QaSpec};

/// What a [`Retranscriber`] heard in the render.
#[derive(Debug, Clone, PartialEq)]
pub struct Retranscription {
    /// The heard words, in order.
    pub words: Vec<String>,
    /// Evidence that says what transcribed the render (model, backend, VAD); copied into the check.
    pub engine: Value,
}

/// Why a [`Retranscriber`] produced no words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetranscribeError {
    /// The speech model or engine is not installed. Prints `SKIP:` and fails the check.
    Unavailable(String),
    /// Any other failure; fails the check with the message as evidence.
    Failed(String),
}

/// Transcribes a rendered file in-process. The host implements it over its speech engine; the
/// check never asks a shared speech service.
pub trait Retranscriber {
    /// Transcribes `render`, using `ffmpeg` for the audio decode.
    fn retranscribe(
        &self,
        ffmpeg: &Path,
        render: &Path,
    ) -> Result<Retranscription, RetranscribeError>;
}

pub const CHECK_ID: &str = "speech.retranscription";

/// How many listed words the message names before it says "and N more".
const LISTED: usize = 8;

/// What the render should say, and how many word errors it may have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetranscribeExpect {
    /// Words the edit keeps, in output order (the source transcript's words, minus cuts).
    pub kept_words: Vec<String>,
    /// Dropped plus extra words tolerated before the check fails.
    pub max_word_errors: usize,
}

/// Lowercase letters, digits and apostrophes. Punctuation, curly quotes and
/// whitespace are dropped, and a token with nothing left is empty.
pub fn normalize(word: &str) -> String {
    word.trim()
        .to_lowercase()
        .replace('\u{2019}', "'")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect()
}

fn normalized(words: &[String]) -> Vec<String> {
    words
        .iter()
        .map(|w| normalize(w))
        .filter(|w| !w.is_empty())
        .collect()
}

/// The alignment of heard words against expected words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordDiff {
    pub expected: usize,
    pub heard: usize,
    pub matched: usize,
    /// Expected words the render did not say, in expected order.
    pub missing: Vec<String>,
    /// Heard words the edit did not expect, in heard order.
    pub extra: Vec<String>,
}

/// Longest-common-subsequence alignment of two normalized word lists.
/// Both inputs must already be normalized.
pub fn diff_words(expected: &[String], heard: &[String]) -> WordDiff {
    let (n, m) = (expected.len(), heard.len());
    let width = m + 1;
    let mut lcs = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if expected[i] == heard[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j, mut matched) = (0usize, 0usize, 0usize);
    let mut missing = Vec::new();
    let mut extra = Vec::new();
    while i < n || j < m {
        if i < n && j < m && expected[i] == heard[j] {
            matched += 1;
            i += 1;
            j += 1;
        } else if j >= m || (i < n && lcs[(i + 1) * width + j] >= lcs[i * width + j + 1]) {
            missing.push(expected[i].clone());
            i += 1;
        } else {
            extra.push(heard[j].clone());
            j += 1;
        }
    }
    WordDiff {
        expected: n,
        heard: m,
        matched,
        missing,
        extra,
    }
}

/// Judges heard words against the expectation. `engine` is the evidence that says what
/// transcribed the render (model, backend, VAD); it is copied into the check's evidence.
pub fn judge(expect: &RetranscribeExpect, heard: &[String], engine: Value) -> QaCheck {
    let expected = normalized(&expect.kept_words);
    let heard = normalized(heard);
    let diff = diff_words(&expected, &heard);
    let errors = diff.missing.len() + diff.extra.len();
    let ok = errors <= expect.max_word_errors;
    let message = if ok {
        format!(
            "render re-transcribes to the {} expected words ({} word errors, {} allowed)",
            diff.expected, errors, expect.max_word_errors
        )
    } else {
        format!(
            "{} expected word(s) missing, {} unexpected word(s) heard (allowed {}): missing {}, extra {}",
            diff.missing.len(),
            diff.extra.len(),
            expect.max_word_errors,
            listed(&diff.missing),
            listed(&diff.extra),
        )
    };
    QaCheck {
        id: CHECK_ID.into(),
        status: if ok {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        severity: Severity::Error,
        message,
        evidence: json!({
            "expected_words": diff.expected,
            "heard_words": diff.heard,
            "matched": diff.matched,
            "missing": diff.missing,
            "extra": diff.extra,
            "max_word_errors": expect.max_word_errors,
            "engine": engine,
        }),
    }
}

fn listed(words: &[String]) -> String {
    if words.is_empty() {
        return "none".into();
    }
    let shown: Vec<&str> = words.iter().take(LISTED).map(String::as_str).collect();
    if words.len() > LISTED {
        format!("{} and {} more", shown.join(" "), words.len() - LISTED)
    } else {
        shown.join(" ")
    }
}

fn failed(message: String, evidence: Value) -> QaCheck {
    QaCheck {
        id: CHECK_ID.into(),
        status: CheckStatus::Fail,
        severity: Severity::Error,
        message,
        evidence,
    }
}

/// Transcribes `render` through `retranscriber` and judges it against `expect`.
///
/// An unavailable speech model prints `SKIP:` and returns a failed check. Any other speech
/// failure also fails the check, with its message as evidence.
pub fn retranscription_check(
    ffmpeg: &Path,
    render: &Path,
    retranscriber: &dyn Retranscriber,
    expect: &RetranscribeExpect,
) -> QaCheck {
    match retranscriber.retranscribe(ffmpeg, render) {
        Ok(heard) => judge(expect, &heard.words, heard.engine),
        Err(RetranscribeError::Unavailable(why)) => {
            eprintln!("SKIP: speech model not installed; {CHECK_ID} not run ({why})");
            failed(
                format!("SKIP: speech model not installed; {CHECK_ID} not run: {why}"),
                json!({"skipped": true, "reason": why}),
            )
        }
        Err(RetranscribeError::Failed(why)) => failed(
            format!("speech re-transcription failed: {why}"),
            json!({"error": why}),
        ),
    }
}

/// Runs the probes and the speech check on `render`, then records the report and
/// receipt (with the check's evidence) in the project. A failing check fails the report.
#[allow(clippy::too_many_arguments)]
pub fn qa_gate_with_retranscription(
    project_dir: &Path,
    tools: &QaTools,
    ffmpeg: &Path,
    render: &Path,
    spec: &QaSpec,
    expect: &QaExpect,
    retranscriber: &dyn Retranscriber,
    speech: &RetranscribeExpect,
) -> Result<QaOutcome, QaError> {
    let report: QaReport = run_qa(tools, render, spec, expect)?;
    let check = retranscription_check(ffmpeg, render, retranscriber, speech);
    record_report(project_dir, report.with_check(check))
}
