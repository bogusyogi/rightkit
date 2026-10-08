//! `rightkit-align`: local script-to-ASR word alignment (RK4, local route).
//!
//! Input is the script as written and the timed words an ASR engine returned
//! (`rightkit_transcript::Word`, which `rightkit-asr` output maps onto through
//! [`asr_words_from_seconds`]). Output is one [`Anchor`] per script token, in
//! script order, and a [`Coverage`] report for designated control-name tokens.
//!
//! Guarantees:
//! - Script-token identity is preserved through pronunciation normalisation.
//! - Each token carries two separate confidences: lexical (did the ASR say the
//!   same word) and timing (how trustworthy the ASR boundaries are).
//! - Mismatched words, unaligned tokens and ambiguous repeats are rejected. A
//!   rejected anchor never satisfies coverage.
//!
//! Dependency choice: a new crate rather than `rightkit-transcript`. The
//! transcript crate is the shared contract with no algorithms, and alignment
//! brings its own normalisation tables, cost model and report types. This crate
//! depends on `rightkit-transcript` only for the `Word`, `Span` and seconds
//! adapter types, and holds no audio, models or I/O.

pub mod normalise;
pub mod timing;

mod solver;

use serde::{Deserialize, Serialize};

use rightkit_transcript::{EndSource, Span, Word};

pub use timing::{
    boundary_errors, Boundary, BoundaryError, LATTICE_MS, MIN_WORD_MS, TIMING_TOLERANCE_MS,
};

use solver::Op;

/// A designated control-name token that must come back accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAnchor {
    /// Index of the script token (whitespace-delimited, zero-based).
    pub token_id: usize,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alignment {
    /// One anchor per script token, in script order. Rejected anchors are kept
    /// with their reason so callers can diagnose them.
    pub anchors: Vec<Anchor>,
    pub coverage: Coverage,
    /// Sum of the integer alignment costs (diagnostic).
    pub total_cost: u32,
    pub script_units: usize,
    pub asr_units: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub token_id: usize,
    pub text: String,
    pub outcome: Outcome,
    /// Minimum over the token's spoken units: 1.0 identical, the similarity for
    /// a substitution, 0.0 unaligned.
    pub lexical_confidence: f32,
    /// Minimum over the matched ASR words of boundary trust (see [`timing`]).
    pub timing_confidence: f32,
    /// Minimum ASR-reported word confidence, when the engine reports one.
    pub asr_confidence: Option<f32>,
    /// What the ASR said at the aligned position, when a word was aligned.
    pub asr_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// The span is the ASR span of the matched words.
    Accepted {
        span: Span,
    },
    Rejected {
        reason: RejectReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The token has no spoken form (punctuation only).
    NoSpokenForm,
    /// No ASR word was aligned to the token (for example it was omitted).
    Unaligned,
    /// An equally good alignment exists elsewhere; the anchor cannot be trusted.
    Ambiguous,
    /// The aligned ASR word differs from the script word (see `asr_text`).
    WordDiffers,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub designated: usize,
    pub accepted: usize,
    /// Designated anchors that were not accepted, with the reason when known.
    pub failures: Vec<CoverageFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageFailure {
    pub token_id: usize,
    pub label: String,
    /// `None` when the token index is outside the script.
    pub reason: Option<RejectReason>,
}

impl Coverage {
    /// The coverage floor: every designated anchor accepted. Vacuously true with no designations.
    pub fn meets_floor(&self) -> bool {
        self.failures.is_empty()
    }
}

impl Alignment {
    pub fn anchor(&self, token_id: usize) -> Option<&Anchor> {
        self.anchors
            .get(token_id)
            .filter(|a| a.token_id == token_id)
    }

    /// The accepted span for a token, or `None` when it was rejected.
    pub fn accepted_span(&self, token_id: usize) -> Option<Span> {
        match self.anchor(token_id)?.outcome {
            Outcome::Accepted { span } => Some(span),
            Outcome::Rejected { .. } => None,
        }
    }
}

/// Adapter from `rightkit-asr` output (`(text, start_s, end_s)`) to contract words.
/// The ASR does not report `end_source` or confidence here, so words are marked
/// as model-timed with no confidence.
pub fn asr_words_from_seconds<'a>(
    items: impl IntoIterator<Item = (&'a str, f32, f32)>,
) -> Vec<Word> {
    rightkit_transcript::words_from_seconds(items)
        .into_iter()
        .map(|w| Word {
            text: w.text,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
            confidence: None,
            end_source: EndSource::Model,
        })
        .collect()
}

/// Aligns `script` to `asr` and reports anchors and control-name coverage.
///
/// Script tokens are the whitespace-delimited pieces of `script`. ASR words
/// must already be in time order, as produced by the engine.
pub fn align(script: &str, asr: &[Word], designated: &[ControlAnchor]) -> Alignment {
    let mut script_units = Vec::new();
    let mut unit_token = Vec::new();
    let tokens: Vec<&str> = script.split_whitespace().collect();
    for (token_id, token) in tokens.iter().enumerate() {
        for unit in normalise::units(token) {
            script_units.push(unit);
            unit_token.push(token_id);
        }
    }

    let mut asr_units = Vec::new();
    let mut unit_word = Vec::new();
    for (word_index, word) in asr.iter().enumerate() {
        for unit in normalise::units(&word.text) {
            asr_units.push(unit);
            unit_word.push(word_index);
        }
    }

    let solution = solver::solve(&script_units, &asr_units);

    // Per script unit: what happened to it in the optimal alignment.
    let mut unit_outcome: Vec<UnitOutcome> = vec![UnitOutcome::Deleted; script_units.len()];
    for op in &solution.ops {
        match *op {
            Op::Match { script, asr } => {
                unit_outcome[script] = UnitOutcome::Matched {
                    asr,
                    similarity: 1.0,
                }
            }
            Op::Sub {
                script,
                asr,
                similarity,
            } => unit_outcome[script] = UnitOutcome::Matched { asr, similarity },
            Op::Del { .. } | Op::Ins { .. } => {}
        }
    }

    let mut token_units: Vec<Vec<usize>> = vec![Vec::new(); tokens.len()];
    for (unit, &token_id) in unit_token.iter().enumerate() {
        token_units[token_id].push(unit);
    }
    let context = AnchorContext {
        unit_outcome: &unit_outcome,
        ambiguous: &solution.ambiguous,
        asr_units: &asr_units,
        unit_word: &unit_word,
        asr,
    };
    let anchors: Vec<Anchor> = tokens
        .iter()
        .enumerate()
        .map(|(token_id, token)| build_anchor(token_id, token, &token_units[token_id], &context))
        .collect();

    let coverage = coverage(designated, &anchors);
    Alignment {
        anchors,
        coverage,
        total_cost: solution.total_cost,
        script_units: script_units.len(),
        asr_units: asr_units.len(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum UnitOutcome {
    Matched { asr: usize, similarity: f32 },
    Deleted,
}

/// Read-only inputs shared by every anchor of one alignment.
struct AnchorContext<'a> {
    unit_outcome: &'a [UnitOutcome],
    ambiguous: &'a [bool],
    asr_units: &'a [String],
    unit_word: &'a [usize],
    asr: &'a [Word],
}

fn build_anchor(token_id: usize, token: &str, units: &[usize], context: &AnchorContext) -> Anchor {
    let AnchorContext {
        unit_outcome,
        ambiguous,
        asr_units,
        unit_word,
        asr,
    } = *context;
    let text = token.to_string();
    if units.is_empty() {
        return Anchor {
            token_id,
            text,
            outcome: Outcome::Rejected {
                reason: RejectReason::NoSpokenForm,
            },
            lexical_confidence: 0.0,
            timing_confidence: 0.0,
            asr_confidence: None,
            asr_text: None,
        };
    }

    let mut lexical = 1.0_f32;
    let mut matched_words: Vec<usize> = Vec::new();
    let mut first_difference: Option<usize> = None;
    let mut any_deleted = false;
    let mut any_ambiguous = false;
    for &u in units {
        match unit_outcome[u] {
            UnitOutcome::Deleted => {
                any_deleted = true;
                lexical = 0.0;
            }
            UnitOutcome::Matched { asr: a, similarity } => {
                lexical = lexical.min(similarity);
                matched_words.push(unit_word[a]);
                if similarity < 1.0 && first_difference.is_none() {
                    first_difference = Some(a);
                }
            }
        }
        any_ambiguous |= ambiguous[u];
    }
    matched_words.sort_unstable();
    matched_words.dedup();

    let timing = matched_words
        .iter()
        .map(|&w| timing::word_timing_confidence(&asr[w]))
        .fold(None, |acc: Option<f32>, c| {
            Some(acc.map_or(c, |m| m.min(c)))
        })
        .unwrap_or(0.0);
    let asr_confidence = matched_words
        .iter()
        .filter_map(|&w| asr[w].confidence)
        .fold(None, |acc: Option<f32>, c| {
            Some(acc.map_or(c, |m| m.min(c)))
        });
    let asr_text = match (any_deleted, first_difference) {
        (true, _) => None,
        (false, Some(a)) => Some(asr_units[a].clone()),
        (false, None) => matched_words.first().map(|&w| asr[w].text.clone()),
    };

    let reason = if any_deleted {
        Some(RejectReason::Unaligned)
    } else if any_ambiguous {
        Some(RejectReason::Ambiguous)
    } else if first_difference.is_some() {
        Some(RejectReason::WordDiffers)
    } else {
        None
    };

    let outcome = match reason {
        Some(reason) => Outcome::Rejected { reason },
        None => {
            let start = matched_words
                .iter()
                .map(|&w| asr[w].start_ms)
                .min()
                .unwrap_or(0);
            let end = matched_words
                .iter()
                .map(|&w| asr[w].end_ms)
                .max()
                .unwrap_or(0);
            Outcome::Accepted {
                span: Span {
                    start_ms: start,
                    end_ms: end,
                },
            }
        }
    };

    Anchor {
        token_id,
        text,
        outcome,
        lexical_confidence: lexical,
        timing_confidence: timing,
        asr_confidence,
        asr_text,
    }
}

fn coverage(designated: &[ControlAnchor], anchors: &[Anchor]) -> Coverage {
    let mut accepted = 0;
    let mut failures = Vec::new();
    for control in designated {
        match anchors.get(control.token_id) {
            Some(anchor) => match anchor.outcome {
                Outcome::Accepted { .. } => accepted += 1,
                Outcome::Rejected { reason } => failures.push(CoverageFailure {
                    token_id: control.token_id,
                    label: control.label.clone(),
                    reason: Some(reason),
                }),
            },
            None => failures.push(CoverageFailure {
                token_id: control.token_id,
                label: control.label.clone(),
                reason: None,
            }),
        }
    }
    Coverage {
        designated: designated.len(),
        accepted,
        failures,
    }
}
