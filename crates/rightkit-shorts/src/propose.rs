//! Candidate windows and the full scoring pipeline.
//!
//! Stages (the old 9-stage scorer, adapted from "accepted editorial candidates" to
//! "windows generated from the transcript"):
//! 1. segmentation: sentences (terminal punctuation, or a pause of at least
//!    `pause_boundary_ms`) and candidate windows of consecutive sentences, trimmed of
//!    leading/trailing fillers, never crossing a pause above `max_internal_pause_ms`;
//! 2. standalone-context validation (dangling opener rejects);
//! 3. hook / payoff (ending quality) / proof / value (incl. pacing) scored separately;
//! 4. duration fit against the primary platform (hard gate);
//! 5. visual support from a supplied reframe track, else neutral at Low confidence;
//! 6. platform fit and brand fit;
//! 7. ranking, overlap suppression and diversity clustering;
//! 8. a cheap preview (poster time + snippet);
//! 9. human selection: the result stays `proposed`; `selected_id` is always `None`.
//!
//! Every window starts at a word start and ends at a word end, so a proposed range is
//! never mid-word; the padded range stays inside the silence around those words.

use std::collections::BTreeSet;

use rightkit_transcript::{Transcript, Word};
use serde::{Deserialize, Serialize};

use crate::audio::{audio_adjustment, window_audio, AudioFeatures};
use crate::budget::BudgetReport;
use crate::config::ShortsProfile;
use crate::error::ShortsError;
use crate::score::*;

pub const PROPOSALS_SCHEMA: &str = "cutright.shorts.proposals.v1";

/// Optional upstream beat label (the old editorial candidate's `beat_label`): a
/// `"hook"` hint overlapping a window's opening sentence earns the hook-label signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeatHint {
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: String,
}

pub struct ProposeInput<'a> {
    pub transcript: &'a Transcript,
    pub hints: &'a [BeatHint],
    /// Reframe-track samples in source time; `None` scores stage 5 neutrally.
    pub visual: Option<&'a [VisualPoint]>,
    /// Aspect ratios of the declared outputs, e.g. `["9:16"]`.
    pub target_aspects: Vec<String>,
    /// Measured loudness and laughter, measured upstream. `None` leaves ranking unchanged.
    pub audio: Option<&'a AudioFeatures>,
}

impl<'a> ProposeInput<'a> {
    pub fn new(transcript: &'a Transcript) -> Self {
        Self {
            transcript,
            hints: &[],
            visual: None,
            target_aspects: vec!["9:16".into()],
            audio: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preview {
    pub poster_ms: u64,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TruthfulnessNote {
    pub reorders_or_omits_material: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlatformFit {
    pub platform: String,
    pub duration_fit: f64,
    pub fits: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    /// Stable: `short-<start_ms>-<end_ms>`.
    pub id: String,
    pub rank: usize,
    pub source_blake3: String,
    /// Word-aligned range: first word start .. last word end (source ms).
    pub start_ms: u64,
    pub end_ms: u64,
    pub duration_ms: u64,
    /// Clip range with silence padding (inside the neighbouring pauses).
    pub clip_start_ms: u64,
    pub clip_end_ms: u64,
    /// Inclusive index range into `Transcript::words`.
    pub first_word: usize,
    pub last_word: usize,
    pub transcript: String,
    pub title: String,
    pub rationale: String,
    pub scores: ScoreBreakdown,
    pub standalone: StandaloneCheck,
    pub truthfulness: TruthfulnessNote,
    pub preview: Preview,
    pub platform_fit: Vec<PlatformFit>,
    pub diversity_cluster: String,
    /// Present when the moment came from the LLM ranker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmEvidence>,
}

/// What the LLM said about a moment, kept beside the heuristic stage scores.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmEvidence {
    pub provider: String,
    pub model: String,
    pub score: f64,
    pub reason: String,
    /// The hook line the model quoted.
    pub hook_text: String,
    /// The quoted hook was found in the validated window.
    pub hook_verified: bool,
    pub start_sentence: String,
    pub end_sentence: String,
    /// Composite before blending with the model score.
    pub heuristic_composite: f64,
}

/// How the proposals were ranked, and why the heuristic scorer was used if it was.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankingInfo {
    /// `"llm"` or `"heuristic"`.
    pub method: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Why the heuristic scorer ran instead of the LLM (no key, provider error, ...).
    pub fallback_reason: Option<String>,
    pub llm_moments_returned: usize,
    pub llm_moments_accepted: usize,
    pub llm_rejections: Vec<String>,
    /// What the token budget did to the LLM request plan; absent on the heuristic path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_budget: Option<BudgetReport>,
}

impl RankingInfo {
    pub(crate) fn heuristic(reason: Option<String>) -> Self {
        Self {
            method: "heuristic".into(),
            provider: None,
            model: None,
            fallback_reason: reason,
            llm_moments_returned: 0,
            llm_moments_accepted: 0,
            llm_rejections: Vec::new(),
            prompt_budget: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectedWindow {
    pub start_ms: u64,
    pub end_ms: u64,
    pub stage: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proposals {
    pub schema: String,
    /// Always `"proposed"`: the pipeline never selects.
    pub status: String,
    pub selected_id: Option<String>,
    pub profile_version: u32,
    pub platform: String,
    pub source_blake3: String,
    pub windows_considered: usize,
    pub suppressed_overlap: usize,
    pub rejected_total: usize,
    /// First `max_reported_rejections` distinct rejections.
    pub rejected: Vec<RejectedWindow>,
    pub ranking: RankingInfo,
    pub proposals: Vec<Proposal>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Sentence {
    pub(crate) first: usize,
    pub(crate) last: usize,
}

pub(crate) fn is_filler(w: &Word, profile: &ShortsProfile) -> bool {
    let n = norm_word(&w.text);
    profile.fillers.contains(&n)
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', ')', ']', '\u{201d}'])
        .ends_with(['.', '?', '!', '\u{2026}'])
}

pub(crate) fn sentences(words: &[Word], profile: &ShortsProfile) -> Vec<Sentence> {
    let mut out = Vec::new();
    let mut first = 0usize;
    for i in 0..words.len() {
        let boundary = i + 1 == words.len()
            || ends_sentence(&words[i].text)
            || words[i + 1].start_ms.saturating_sub(words[i].end_ms) as i64
                >= profile.windows.pause_boundary_ms;
        if boundary {
            out.push(Sentence { first, last: i });
            first = i + 1;
        }
    }
    out
}

pub(crate) fn trimmed_range(
    s: Sentence,
    words: &[Word],
    profile: &ShortsProfile,
) -> Option<(usize, usize)> {
    let first = (s.first..=s.last).find(|&i| !is_filler(&words[i], profile))?;
    let last = (s.first..=s.last)
        .rev()
        .find(|&i| !is_filler(&words[i], profile))?;
    Some((first, last))
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A title from the transcript: the sentence in the window with the most hook cues
/// (earliest on ties), fillers and a dangling opener dropped, trailing punctuation
/// removed (a question mark is kept), cut to `title.max_chars` at a word boundary.
fn suggest_title(words: &[Word], sents: &[Sentence], profile: &ShortsProfile) -> String {
    let mut best: Option<(usize, String)> = None;
    for s in sents {
        let sw: Vec<&Word> = words[s.first..=s.last]
            .iter()
            .filter(|w| !is_filler(w, profile))
            .collect();
        if sw.is_empty() {
            continue;
        }
        let text = sw
            .iter()
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ");
        let cues = contains_cue(&text.to_lowercase(), &profile.hook_cues).len();
        if best.as_ref().is_none_or(|(c, _)| cues > *c) {
            best = Some((cues, text));
        }
    }
    let Some((_, mut text)) = best else {
        return String::new();
    };
    // Drop leading dangling openers ("so the first thing..." -> "the first thing...").
    while let Some(first) = first_word(&text) {
        if profile.dangling_openers.contains(&first) && text.split_whitespace().count() > 3 {
            text = text
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ");
        } else {
            break;
        }
    }
    let question = text.trim_end().ends_with('?');
    let mut t = text
        .trim_end_matches(['.', ',', ';', ':', '!', '?', '\u{2026}'])
        .trim()
        .to_string();
    let max = profile.title.max_chars;
    let reserve = usize::from(question);
    if t.chars().count() + reserve > max {
        let mut cut = String::new();
        for w in t.split_whitespace() {
            let next = if cut.is_empty() {
                w.to_string()
            } else {
                format!("{cut} {w}")
            };
            if next.chars().count() + reserve > max {
                break;
            }
            cut = next;
        }
        t = cut;
    }
    let mut chars = t.chars();
    let mut title = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => return String::new(),
    };
    if question {
        title.push('?');
    }
    title
}

pub(crate) struct Ctx<'a> {
    pub input: &'a ProposeInput<'a>,
    pub profile: &'a ShortsProfile,
    pub sents: Vec<Sentence>,
}

/// Scores one window (sentences `si..=sj`, words `first..=last`). The hard gates
/// (standalone context, duration) come back as a rejection.
pub(crate) fn score_window(
    cx: &Ctx<'_>,
    si: usize,
    sj: usize,
    first: usize,
    last: usize,
) -> Result<Proposal, RejectedWindow> {
    let (profile, tr) = (cx.profile, cx.input.transcript);
    let words = &tr.words;
    let primary = profile.primary();
    let (start, end) = (words[first].start_ms, words[last].end_ms);
    let dur = (end - start) as i64;
    let slice = &words[first..=last];
    let text = join(slice);
    let reject = |stage: &str, reason: String| RejectedWindow {
        start_ms: start,
        end_ms: end,
        stage: stage.into(),
        reason,
    };
    let standalone = validate_standalone(&text, profile);
    if !standalone.passed {
        return Err(reject("standalone_context", standalone.reasons.join("; ")));
    }
    let duration_fit = score_duration_fit(dur, primary);
    if duration_fit.value <= 0.0 {
        return Err(reject("duration_fit", duration_fit.rationale));
    }
    let refs: Vec<&Word> = slice.iter().collect();
    let fillers = refs.iter().filter(|w| is_filler(w, profile)).count();
    let open_end = words[cx.sents[si].last].end_ms;
    let labeled = cx
        .input
        .hints
        .iter()
        .any(|h| h.label == "hook" && h.start_ms < open_end && h.end_ms > start);
    let visual = cx.input.visual.map(|pts| {
        pts.iter()
            .filter(|p| p.source_ms >= start && p.source_ms < end)
            .collect::<Vec<_>>()
    });
    let partial = ScoreBreakdown {
        hook: score_hook(&text, labeled, profile),
        payoff: score_payoff(&text, profile),
        proof: score_proof(&text, profile),
        value: score_value(&refs, fillers, dur, profile),
        duration_fit,
        visual_support: score_visual_support(visual.as_deref(), &profile.visual_support),
        platform_fit: score_platform_fit(&cx.input.target_aspects),
        brand_fit: score_brand_fit(&text, profile),
        composite: 0.0,
    };
    let heuristic_composite = composite_score(&partial, &profile.weights);
    let (composite, audio_note) = match cx.input.audio {
        Some(features) => {
            let adjustment = audio_adjustment(&window_audio(features, start, end), &profile.audio);
            (
                q6((heuristic_composite + adjustment.delta).clamp(0.0, 1.0)),
                Some(adjustment.rationale),
            )
        }
        None => (heuristic_composite, None),
    };
    let scores = ScoreBreakdown {
        composite,
        ..partial
    };

    // Silence padding, never past halfway to the neighbouring word.
    let prev_end = if first == 0 {
        0
    } else {
        words[first - 1].end_ms
    };
    let next_start = words.get(last + 1).map_or(tr.duration_ms, |w| w.start_ms);
    let pad = profile.windows.pad_ms.max(0) as u64;
    let clip_start = start - pad.min((start - prev_end) / 2);
    let clip_end = end + pad.min((next_start - end) / 2);

    let pauses: u64 = (first..last)
        .map(|k| words[k + 1].start_ms.saturating_sub(words[k].end_ms))
        .filter(|g| *g as i64 >= profile.windows.pause_boundary_ms / 3)
        .sum();
    let truthfulness = TruthfulnessNote {
        reorders_or_omits_material: false,
        note: format!(
            "one contiguous source range; nothing is reordered. A tight cut may drop \
             {fillers} filler word(s) and up to {pauses}ms of pauses inside it; the \
             cut plan lists every removal"
        ),
    };
    let poster_ms = slice
        .iter()
        .filter(|w| w.start_ms - start <= 2_000)
        .max_by(|a, b| {
            let c = |w: &Word| {
                w.confidence
                    .map_or(profile.assumed_word_confidence, f64::from)
            };
            c(a).total_cmp(&c(b))
        })
        .map_or(start, |w| w.start_ms);
    let preview = Preview {
        poster_ms,
        snippet: text
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" "),
    };
    let platform_fit = profile
        .platforms
        .iter()
        .map(|p| {
            let s = score_duration_fit(dur, p);
            PlatformFit {
                platform: p.id.clone(),
                duration_fit: s.value,
                fits: s.value > 0.0,
            }
        })
        .collect();
    let mut rationale = format!(
        "hook {:.2}, payoff {:.2}, proof {:.2}, value {:.2}, duration_fit {:.2}, \
         visual_support {:.2}, platform_fit {:.2}, brand_fit {:.2} -> composite {composite:.2}",
        scores.hook.value,
        scores.payoff.value,
        scores.proof.value,
        scores.value.value,
        scores.duration_fit.value,
        scores.visual_support.value,
        scores.platform_fit.value,
        scores.brand_fit.value,
    );
    if let Some(note) = audio_note {
        rationale.push_str("; ");
        rationale.push_str(&note);
    }
    Ok(Proposal {
        id: format!("short-{start}-{end}"),
        rank: 0,
        source_blake3: tr.source_blake3.clone(),
        start_ms: start,
        end_ms: end,
        duration_ms: end - start,
        clip_start_ms: clip_start,
        clip_end_ms: clip_end,
        first_word: first,
        last_word: last,
        title: suggest_title(words, &cx.sents[si..=sj], profile),
        transcript: text,
        rationale,
        scores,
        standalone,
        truthfulness,
        preview,
        platform_fit,
        diversity_cluster: String::new(),
        llm: None,
    })
}

/// Ranking, overlap suppression and diversity clustering over scored proposals.
/// Returns (selected proposals, overlap-suppressed count).
pub(crate) fn finish(
    mut scored: Vec<Proposal>,
    profile: &ShortsProfile,
    count: usize,
) -> (Vec<Proposal>, usize) {
    // Deterministic ranking: composite desc, then earlier start, then shorter.
    scored.sort_by(|a, b| {
        b.scores
            .composite
            .total_cmp(&a.scores.composite)
            .then(a.start_ms.cmp(&b.start_ms))
            .then(a.end_ms.cmp(&b.end_ms))
    });
    let mut kept: Vec<Proposal> = Vec::new();
    let mut suppressed = 0usize;
    for p in scored {
        let clash = kept.iter().any(|k| {
            let ov = k
                .end_ms
                .min(p.end_ms)
                .saturating_sub(k.start_ms.max(p.start_ms));
            ov as f64 / k.duration_ms.min(p.duration_ms).max(1) as f64
                > profile.dedupe.max_overlap_fraction
        });
        if clash {
            suppressed += 1;
        } else {
            kept.push(p);
        }
    }
    let texts: Vec<&str> = kept.iter().map(|p| p.transcript.as_str()).collect();
    let (cluster_of, selected) =
        cluster_and_select(&texts, profile.dedupe.diversity_similarity_threshold, count);
    let mut proposals = Vec::with_capacity(selected.len());
    for (rank, idx) in selected.into_iter().enumerate() {
        let mut p = kept[idx].clone();
        p.rank = rank + 1;
        p.diversity_cluster = format!("cluster-{:03}", cluster_of[idx]);
        proposals.push(p);
    }
    (proposals, suppressed)
}

/// Runs the heuristic pipeline and returns up to `count` proposals.
pub fn propose_shorts(
    input: &ProposeInput<'_>,
    profile: &ShortsProfile,
    count: usize,
) -> Result<Proposals, ShortsError> {
    propose_heuristic(input, profile, count, RankingInfo::heuristic(None))
}

pub(crate) fn propose_heuristic(
    input: &ProposeInput<'_>,
    profile: &ShortsProfile,
    count: usize,
    ranking: RankingInfo,
) -> Result<Proposals, ShortsError> {
    profile.validate()?;
    let tr = input.transcript;
    tr.validate()?;
    if let Some(features) = input.audio {
        features.validate()?;
    }
    let words = &tr.words;
    let primary = profile.primary();
    let cx = Ctx {
        input,
        profile,
        sents: sentences(words, profile),
    };

    let mut rejected: Vec<RejectedWindow> = Vec::new();
    let mut rejected_total = 0usize;
    let mut rejected_starts: BTreeSet<(usize, String)> = BTreeSet::new();
    let mut windows_considered = 0usize;
    let mut scored: Vec<Proposal> = Vec::new();

    'outer: for i in 0..cx.sents.len() {
        let Some((first, _)) = trimmed_range(cx.sents[i], words, profile) else {
            continue;
        };
        for j in i..cx.sents.len() {
            if j > i {
                let pause = words[cx.sents[j].first]
                    .start_ms
                    .saturating_sub(words[cx.sents[j - 1].last].end_ms)
                    as i64;
                if pause > profile.windows.max_internal_pause_ms {
                    break;
                }
            }
            let Some((_, last)) = trimmed_range(cx.sents[j], words, profile) else {
                continue;
            };
            let dur = (words[last].end_ms - words[first].start_ms) as i64;
            if dur > primary.max_duration_ms {
                break;
            }
            if dur < primary.min_duration_ms {
                continue;
            }
            if windows_considered >= profile.windows.max_windows {
                break 'outer;
            }
            windows_considered += 1;
            match score_window(&cx, i, j, first, last) {
                Ok(p) => scored.push(p),
                Err(r) => {
                    if rejected_starts.insert((first, r.stage.clone())) {
                        rejected_total += 1;
                        if rejected.len() < profile.max_reported_rejections {
                            rejected.push(r);
                        }
                    }
                }
            }
        }
    }

    let (proposals, suppressed_overlap) = finish(scored, profile, count);
    Ok(Proposals {
        schema: PROPOSALS_SCHEMA.into(),
        status: "proposed".into(),
        selected_id: None,
        profile_version: profile.profile_version,
        platform: profile.primary_platform.clone(),
        source_blake3: tr.source_blake3.clone(),
        windows_considered,
        suppressed_overlap,
        rejected_total,
        rejected,
        ranking,
        proposals,
    })
}
