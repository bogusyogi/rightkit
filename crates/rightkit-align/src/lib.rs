//! `rightkit-align`: local script-to-ASR word alignment (RK4, local route).
//!
//! Input is a [`ScriptSpec`] (the script as addressable tokens) and the timed
//! words an ASR engine returned (`rightkit_transcript::Word`, which
//! `rightkit-asr` output maps onto through [`asr_words_from_seconds`]). Output
//! is an [`Alignment`]: one [`Anchor`] per script token in script order, one
//! [`Clause`] per clause that can be timed, and a [`Coverage`] report for
//! designated targets. [`Alignment::to_contract`] yields the versioned
//! `rightkit.alignment` v1 document (see [`contract`]).
//!
//! Guarantees:
//! - Every row echoes its `word_id`, `step_id` and, when designated, its
//!   `target_id`. Rows are never identified by label text.
//! - Each token carries two separate confidences: lexical (did the ASR say the
//!   same word) and timing (the formula is in [`timing`]).
//! - Each accepted span is given in milliseconds and in ASR samples at the
//!   sample rate passed to [`align`].
//! - Mismatched words, unaligned tokens and ambiguous repeats are rejected. A
//!   rejected anchor never satisfies coverage.
//!
//! Dependency choice: a new crate rather than `rightkit-transcript`. The
//! transcript crate is the shared contract with no algorithms, and alignment
//! brings its own normalisation tables, cost model and report types. This crate
//! depends on `rightkit-transcript` only for the `Word` and seconds adapter
//! types, and holds no audio, models or I/O.

pub mod contract;
pub mod normalise;
pub mod script;
pub mod timing;

mod solver;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use rightkit_transcript::{EndSource, Word};

pub use contract::{AlignmentContract, Route, CONTRACT, CONTRACT_VERSION};
pub use script::{ScriptError, ScriptSpec, ScriptToken};
pub use timing::{
    boundary_errors, Boundary, BoundaryError, LATTICE_MS, MIN_WORD_MS, TIMING_TOLERANCE_MS,
};

use solver::Op;

/// Acceptance tolerance and lattice assumption for one alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignOptions {
    /// Boundary tolerance in milliseconds. Recorded on the [`Alignment`] and
    /// used by [`boundary_errors`]. Default 50 (R8).
    pub tolerance_ms: u64,
    /// Frame step, in milliseconds, of the lattice that produced model-timed word
    /// boundaries. Parakeet is 80. `None` asserts continuous boundaries. Default
    /// `Some(80)`, so an unset option never reports the flat 1.0 a lattice cannot
    /// support. See [`timing`] for the formula.
    pub lattice_ms: Option<u32>,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self {
            tolerance_ms: TIMING_TOLERANCE_MS,
            lattice_ms: Some(LATTICE_MS as u32),
        }
    }
}

/// A script token that must come back accepted, identified by its `word_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignatedAnchor {
    pub word_id: String,
    pub target_id: String,
}

/// A time range in milliseconds and in ASR samples at `sample_rate`.
///
/// Samples are `floor(ms * sample_rate / 1000)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_rate: u32,
}

impl Span {
    /// Builds a span from milliseconds, deriving the sample positions.
    pub fn from_ms(start_ms: u64, end_ms: u64, sample_rate: u32) -> Span {
        Span {
            start_ms,
            end_ms,
            start_sample: ms_to_samples(start_ms, sample_rate),
            end_sample: ms_to_samples(end_ms, sample_rate),
            sample_rate,
        }
    }
}

fn ms_to_samples(ms: u64, sample_rate: u32) -> u64 {
    u64::try_from(u128::from(ms) * u128::from(sample_rate) / 1000).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// Tolerance this alignment was produced with, in milliseconds.
    pub timing_tolerance_ms: u64,
    /// One anchor per script token, in script order. Rejected anchors are kept
    /// with their reason so callers can diagnose them.
    pub anchors: Vec<Anchor>,
    /// Timed clauses. A clause is emitted only when both its first and last
    /// spoken tokens are accepted.
    pub clauses: Vec<Clause>,
    pub coverage: Coverage,
    /// Sum of the integer alignment costs (diagnostic).
    pub total_cost: u32,
    pub script_units: usize,
    pub asr_units: usize,
}

/// The outcome for one script token. Every row echoes its identities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub word_id: String,
    pub step_id: String,
    /// The `target_id` of the designation for this token, when it is designated.
    pub target_id: Option<String>,
    /// The token text as written, without any trailing newline.
    pub text: String,
    pub outcome: Outcome,
    /// Minimum over the token's spoken units: 1.0 identical, the similarity for
    /// a substitution, 0.0 unaligned.
    pub lexical_confidence: f32,
    /// Minimum over the matched ASR words of boundary trust, times
    /// `lexical_confidence`. Derivation in [`timing`].
    pub timing_confidence: f32,
    /// Minimum ASR-reported word confidence, when the engine reports one.
    pub asr_confidence: Option<f32>,
    /// What the ASR said at the aligned position, when a word was aligned.
    pub asr_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// The span is the ASR span of the matched words.
    Accepted {
        #[serde(flatten)]
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

/// A run of script tokens between punctuation or a step boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clause {
    /// `c{n}`, where `n` counts every clause group in the script, so an id is
    /// stable even when an earlier group is not emitted.
    pub clause_id: String,
    pub step_id: String,
    /// The first and last spoken tokens of the clause. Punctuation-only tokens
    /// are skipped.
    pub first_word_id: String,
    pub last_word_id: String,
    /// The span of `first_word_id`. Its `start_ms` is the clause start.
    pub start: Span,
    /// The span of `last_word_id`. Its `end_ms` is the clause end.
    pub end: Span,
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
    pub word_id: String,
    pub target_id: String,
    /// `None` when the `word_id` is not in the script.
    pub reason: Option<RejectReason>,
}

impl Coverage {
    /// The coverage floor: every designated anchor accepted. Vacuously true with no designations.
    pub fn meets_floor(&self) -> bool {
        self.failures.is_empty()
    }
}

impl Alignment {
    /// The anchor for a `word_id`, or `None` when the script has no such token.
    pub fn anchor(&self, word_id: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|a| a.word_id == word_id)
    }

    /// The accepted span for a `word_id`, or `None` when it was rejected or absent.
    pub fn accepted_span(&self, word_id: &str) -> Option<Span> {
        match self.anchor(word_id)?.outcome {
            Outcome::Accepted { span } => Some(span),
            Outcome::Rejected { .. } => None,
        }
    }

    /// The versioned `rightkit.alignment` v1 document for this alignment.
    pub fn to_contract(&self) -> AlignmentContract {
        AlignmentContract {
            contract: contract::ContractTag,
            contract_version: contract::VersionTag,
            route: Route::Local,
            tokens: self.anchors.clone(),
            clauses: self.clauses.clone(),
            coverage: self.coverage.clone(),
            timing_tolerance_ms: self.timing_tolerance_ms,
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

/// Aligns `script` to `asr` and reports anchors, clauses and designated coverage.
///
/// `asr` must already be in time order, as the engine produced it, and
/// `asr_sample_rate` is the rate of the audio those times refer to.
///
/// # Panics
///
/// Panics when `asr_sample_rate` is zero, when `options.lattice_ms` is
/// `Some(0)`, or when [`ScriptSpec::validate`] fails. Those are caller bugs, not
/// audio conditions.
pub fn align(
    script: &ScriptSpec,
    asr: &[Word],
    asr_sample_rate: u32,
    designated: &[DesignatedAnchor],
    options: &AlignOptions,
) -> Alignment {
    assert!(
        asr_sample_rate > 0,
        "rightkit-align: asr_sample_rate must be positive"
    );
    assert!(
        options.lattice_ms != Some(0),
        "rightkit-align: lattice_ms must be positive"
    );
    if let Err(error) = script.validate() {
        panic!("rightkit-align: invalid script: {error}");
    }

    let mut script_units = Vec::new();
    let mut unit_token = Vec::new();
    for (token_index, token) in script.tokens.iter().enumerate() {
        for unit in normalise::units(&token.text) {
            script_units.push(unit);
            unit_token.push(token_index);
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

    let mut token_units: Vec<Vec<usize>> = vec![Vec::new(); script.tokens.len()];
    for (unit, &token_index) in unit_token.iter().enumerate() {
        token_units[token_index].push(unit);
    }

    // First designation wins when a word_id is designated twice.
    let mut targets: HashMap<&str, &str> = HashMap::new();
    for designation in designated {
        targets
            .entry(designation.word_id.as_str())
            .or_insert(designation.target_id.as_str());
    }

    let context = AnchorContext {
        unit_outcome: &unit_outcome,
        ambiguous: &solution.ambiguous,
        asr_units: &asr_units,
        unit_word: &unit_word,
        asr,
        sample_rate: asr_sample_rate,
        options,
    };
    let anchors: Vec<Anchor> = script
        .tokens
        .iter()
        .enumerate()
        .map(|(token_index, token)| {
            let target = targets.get(token.word_id.as_str()).copied();
            build_anchor(token, target, &token_units[token_index], &context)
        })
        .collect();

    let clauses = build_clauses(script, &anchors);
    let coverage = coverage(designated, &anchors);
    Alignment {
        timing_tolerance_ms: options.tolerance_ms,
        anchors,
        clauses,
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
    sample_rate: u32,
    options: &'a AlignOptions,
}

fn build_anchor(
    token: &ScriptToken,
    target_id: Option<&str>,
    units: &[usize],
    context: &AnchorContext,
) -> Anchor {
    let AnchorContext {
        unit_outcome,
        ambiguous,
        asr_units,
        unit_word,
        asr,
        sample_rate,
        options,
    } = *context;
    let text = token.text.trim().to_string();
    let target_id = target_id.map(str::to_string);
    if units.is_empty() {
        return Anchor {
            word_id: token.word_id.clone(),
            step_id: token.step_id.clone(),
            target_id,
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

    let boundary_trust = matched_words
        .iter()
        .map(|&w| timing::word_timing_confidence(&asr[w], options))
        .fold(None, |acc: Option<f32>, c| {
            Some(acc.map_or(c, |m| m.min(c)))
        })
        .unwrap_or(0.0);
    let timing_confidence = boundary_trust * lexical;
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
                span: Span::from_ms(start, end, sample_rate),
            }
        }
    };

    Anchor {
        word_id: token.word_id.clone(),
        step_id: token.step_id.clone(),
        target_id,
        text,
        outcome,
        lexical_confidence: lexical,
        timing_confidence,
        asr_confidence,
        asr_text,
    }
}

/// Groups tokens into clauses. A group ends after a token that ends a clause on
/// its text, before a step change, and at the end of the script.
fn build_clauses(script: &ScriptSpec, anchors: &[Anchor]) -> Vec<Clause> {
    let mut clauses = Vec::new();
    let mut group: Vec<usize> = Vec::new();
    let mut group_index = 0usize;
    for (index, token) in script.tokens.iter().enumerate() {
        group.push(index);
        let step_changes = script
            .tokens
            .get(index + 1)
            .is_none_or(|next| next.step_id != token.step_id);
        if step_changes || script::ends_clause(&token.text) {
            if let Some(clause) = clause_from_group(&group, group_index, script, anchors) {
                clauses.push(clause);
            }
            group.clear();
            group_index += 1;
        }
    }
    clauses
}

fn clause_from_group(
    group: &[usize],
    index: usize,
    script: &ScriptSpec,
    anchors: &[Anchor],
) -> Option<Clause> {
    let spoken: Vec<usize> = group
        .iter()
        .copied()
        .filter(|&i| {
            !matches!(
                anchors[i].outcome,
                Outcome::Rejected {
                    reason: RejectReason::NoSpokenForm
                }
            )
        })
        .collect();
    let (&first, &last) = (spoken.first()?, spoken.last()?);
    let start = match anchors[first].outcome {
        Outcome::Accepted { span } => span,
        Outcome::Rejected { .. } => return None,
    };
    let end = match anchors[last].outcome {
        Outcome::Accepted { span } => span,
        Outcome::Rejected { .. } => return None,
    };
    Some(Clause {
        clause_id: format!("c{index}"),
        step_id: script.tokens[first].step_id.clone(),
        first_word_id: anchors[first].word_id.clone(),
        last_word_id: anchors[last].word_id.clone(),
        start,
        end,
    })
}

fn coverage(designated: &[DesignatedAnchor], anchors: &[Anchor]) -> Coverage {
    let index: HashMap<&str, &Anchor> = anchors.iter().map(|a| (a.word_id.as_str(), a)).collect();
    let mut accepted = 0;
    let mut failures = Vec::new();
    for designation in designated {
        let reason = match index.get(designation.word_id.as_str()) {
            Some(anchor) => match anchor.outcome {
                Outcome::Accepted { .. } => {
                    accepted += 1;
                    continue;
                }
                Outcome::Rejected { reason } => Some(reason),
            },
            None => None,
        };
        failures.push(CoverageFailure {
            word_id: designation.word_id.clone(),
            target_id: designation.target_id.clone(),
            reason,
        });
    }
    Coverage {
        designated: designated.len(),
        accepted,
        failures,
    }
}
