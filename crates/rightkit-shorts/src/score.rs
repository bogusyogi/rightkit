//! The scoring stages, ported from `video-project/src/shorts_scoring.rs`
//! (REV2 plan §15.4). Same formulas; weights and cue lists come from the
//! [`ShortsProfile`] JSON. Every stage records value, confidence and rationale.

use std::collections::HashSet;

use rightkit_transcript::Word;
use serde::{Deserialize, Serialize};

use crate::config::{PlatformWindow, ShortsProfile, VisualSupportProfile};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Quantize a persisted score to 6 decimals. serde_json's f64 parser can be one
/// ulp off on long mantissas, so full-precision values would not round-trip.
pub(crate) fn q6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Score {
    pub value: f64,
    pub confidence: Confidence,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreBreakdown {
    pub hook: Score,
    pub payoff: Score,
    pub proof: Score,
    pub value: Score,
    pub duration_fit: Score,
    pub visual_support: Score,
    pub platform_fit: Score,
    pub brand_fit: Score,
    pub composite: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StandaloneCheck {
    pub passed: bool,
    pub confidence: Confidence,
    pub reasons: Vec<String>,
}

/// One tracked-subject sample (the reframe lane's track), in SOURCE milliseconds.
/// Stage 5 reads real evidence from these when supplied.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct VisualPoint {
    pub source_ms: u64,
    pub center_x: f64,
    pub center_y: f64,
    pub confidence: f64,
    pub gap: bool,
    pub shot_boundary: bool,
}

/// Lower-cased word with surrounding punctuation removed (apostrophes kept).
pub(crate) fn norm_word(t: &str) -> String {
    t.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

/// First non-empty normalised word of `text`.
pub(crate) fn first_word(text: &str) -> Option<String> {
    text.split_whitespace()
        .next()
        .map(norm_word)
        .filter(|t| !t.is_empty())
}

/// Stage 2: standalone-context validation. A dangling opener fails at High
/// confidence; passing is Medium (a lexicon cannot prove self-containedness).
pub fn validate_standalone(text: &str, profile: &ShortsProfile) -> StandaloneCheck {
    match first_word(text) {
        Some(opener) if profile.dangling_openers.contains(&opener) => StandaloneCheck {
            passed: false,
            confidence: Confidence::High,
            reasons: vec![format!(
                "opens on \"{opener}\", a dangling reference to context this clip does not contain"
            )],
        },
        Some(_) => StandaloneCheck {
            passed: true,
            confidence: Confidence::Medium,
            reasons: vec!["no known dangling-reference opener detected".into()],
        },
        None => StandaloneCheck {
            passed: false,
            confidence: Confidence::High,
            reasons: vec!["empty transcript text".into()],
        },
    }
}

/// Cues present in `lower_text`. Matching is on word boundaries ("so" does not match
/// inside "also"); the last word of a cue of 4+ letters also matches its inflections
/// ("mistake" matches "mistakes"). The old scorer matched raw substrings.
pub(crate) fn contains_cue(lower_text: &str, cues: &[String]) -> Vec<String> {
    let hay: Vec<String> = lower_text.split_whitespace().map(norm_word).collect();
    cues.iter()
        .filter(|cue| {
            let p: Vec<String> = cue.split_whitespace().map(norm_word).collect();
            !p.is_empty()
                && p.len() <= hay.len()
                && hay.windows(p.len()).any(|w| {
                    let last = p.len() - 1;
                    w[..last] == p[..last]
                        && (w[last] == p[last]
                            || (p[last].len() >= 4 && w[last].starts_with(p[last].as_str())))
                })
        })
        .cloned()
        .collect()
}

/// Stage 3a: hook. Question mark, hook-cue lexicon, upstream "hook" label.
pub fn score_hook(text: &str, labeled_hook: bool, profile: &ShortsProfile) -> Score {
    let sw = &profile.signal_weights;
    let lower = text.to_lowercase();
    let has_question = text.contains('?');
    let cues = contains_cue(&lower, &profile.hook_cues);
    let mut value = 0.0;
    let mut signals = Vec::new();
    if has_question {
        value += sw.hook_question;
        signals.push("contains a question".to_string());
    }
    if !cues.is_empty() {
        value += sw.hook_cue;
        signals.push(format!("hook cue(s): {}", cues.join(", ")));
    }
    if labeled_hook {
        value += sw.hook_labeled;
        signals.push("upstream beat label is \"hook\"".to_string());
    }
    let confidence = if has_question || labeled_hook {
        Confidence::High
    } else {
        Confidence::Medium
    };
    Score {
        value: q6(value.min(1.0)),
        confidence,
        rationale: if signals.is_empty() {
            "no hook signal detected (no question mark, hook-cue lexicon match, or upstream hook label)"
                .into()
        } else {
            signals.join("; ")
        },
    }
}

/// Stage 3b: payoff / ending quality. Terminal punctuation, a payoff cue in the last
/// 12 words, and not trailing on a coordinating conjunction.
pub fn score_payoff(text: &str, profile: &ShortsProfile) -> Score {
    let sw = &profile.signal_weights;
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    let ends_complete = trimmed.ends_with(['.', '!', '?']);
    let tail = lower
        .split_whitespace()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ");
    let cues = contains_cue(&tail, &profile.payoff_cues);
    let last_word = lower
        .split_whitespace()
        .last()
        .map(norm_word)
        .unwrap_or_default();
    let trails = ["and", "but", "so", "because", "which"].contains(&last_word.as_str());
    let mut value = 0.0;
    let mut signals = Vec::new();
    if ends_complete {
        value += sw.payoff_terminal;
        signals.push("ends on terminal punctuation".to_string());
    }
    if !cues.is_empty() {
        value += sw.payoff_cue;
        signals.push(format!("payoff cue(s): {}", cues.join(", ")));
    }
    if !trails {
        value += sw.payoff_no_trailing_conjunction;
    } else {
        signals.push(format!("trails on \"{last_word}\"; likely mid-clause"));
    }
    Score {
        value: q6(value.min(1.0)),
        confidence: if ends_complete {
            Confidence::High
        } else {
            Confidence::Medium
        },
        rationale: if signals.is_empty() {
            "no payoff signal detected".into()
        } else {
            signals.join("; ")
        },
    }
}

/// Stage 3c: proof. Digits and proof cues; always Low confidence (lexicon only).
pub fn score_proof(text: &str, profile: &ShortsProfile) -> Score {
    let sw = &profile.signal_weights;
    let lower = text.to_lowercase();
    let has_digit = text.chars().any(|c| c.is_ascii_digit());
    let cues = contains_cue(&lower, &profile.proof_cues);
    let mut value = 0.0;
    let mut signals = Vec::new();
    if has_digit {
        value += sw.proof_digit;
        signals.push("contains a number".to_string());
    }
    if !cues.is_empty() {
        value += sw.proof_cue;
        signals.push(format!("proof cue(s): {}", cues.join(", ")));
    }
    Score {
        value: q6(value.min(1.0)),
        confidence: Confidence::Low,
        rationale: if signals.is_empty() {
            "no proof signal detected (lexicon-only heuristic; may under-detect real evidence)"
                .into()
        } else {
            format!(
                "{} (lexicon-only heuristic; does not verify the claim is actually substantiated)",
                signals.join("; ")
            )
        },
    }
}

/// Stage 3d: value / information density and pacing, from real transcript statistics:
/// unique-word ratio, average word confidence, speaking rate vs the ideal band, and
/// filler ratio.
pub fn score_value(
    words: &[&Word],
    filler_count: usize,
    duration_ms: i64,
    profile: &ShortsProfile,
) -> Score {
    if words.is_empty() {
        return Score {
            value: 0.0,
            confidence: Confidence::High,
            rationale: "no transcript words found in this window".into(),
        };
    }
    let (sw, pc) = (&profile.signal_weights, &profile.pacing);
    let n = words.len() as f64;
    let unique = words
        .iter()
        .map(|w| norm_word(&w.text))
        .collect::<HashSet<_>>()
        .len() as f64;
    let unique_ratio = unique / n;
    let have_conf = words.iter().any(|w| w.confidence.is_some());
    let avg_conf = words
        .iter()
        .map(|w| {
            w.confidence
                .map_or(profile.assumed_word_confidence, f64::from)
        })
        .sum::<f64>()
        / n;
    let seconds = duration_ms.max(1) as f64 / 1000.0;
    let wps = n / seconds;
    let pacing = if (pc.ideal_min_wps..=pc.ideal_max_wps).contains(&wps) {
        1.0
    } else if wps < pc.ideal_min_wps {
        (wps / pc.ideal_min_wps).clamp(0.0, 1.0)
    } else {
        (1.0 - (wps - pc.ideal_max_wps) / pc.decay_span_wps).clamp(0.0, 1.0)
    };
    let filler_ratio = filler_count as f64 / n;
    let filler_score = (1.0 - filler_ratio).clamp(0.0, 1.0);
    let total = sw.value_unique_ratio + sw.value_confidence + sw.value_pacing + sw.value_filler;
    let raw = unique_ratio * sw.value_unique_ratio
        + avg_conf * sw.value_confidence
        + pacing * sw.value_pacing
        + filler_score * sw.value_filler;
    Score {
        value: q6(if total > 0.0 { raw / total } else { 0.0 }.clamp(0.0, 1.0)),
        confidence: if have_conf {
            Confidence::High
        } else {
            Confidence::Medium
        },
        rationale: format!(
            "unique-word ratio {unique_ratio:.2}, avg word confidence {avg_conf:.2}{}, \
             speaking rate {wps:.2} wps (pacing {pacing:.2}), filler ratio {filler_ratio:.2}",
            if have_conf { "" } else { " (assumed)" }
        ),
    }
}

/// Stage 4: duration fit against a platform window; zero outside the hard bounds.
pub fn score_duration_fit(duration_ms: i64, platform: &PlatformWindow) -> Score {
    if duration_ms < platform.min_duration_ms || duration_ms > platform.max_duration_ms {
        return Score {
            value: 0.0,
            confidence: Confidence::High,
            rationale: format!(
                "{duration_ms}ms is outside the hard {} window [{}, {}]ms",
                platform.id, platform.min_duration_ms, platform.max_duration_ms
            ),
        };
    }
    let value = if duration_ms >= platform.ideal_min_ms && duration_ms <= platform.ideal_max_ms {
        1.0
    } else if duration_ms < platform.ideal_min_ms {
        let span = (platform.ideal_min_ms - platform.min_duration_ms).max(1) as f64;
        ((duration_ms - platform.min_duration_ms) as f64 / span).clamp(0.0, 1.0)
    } else {
        let span = (platform.max_duration_ms - platform.ideal_max_ms).max(1) as f64;
        (1.0 - (duration_ms - platform.ideal_max_ms) as f64 / span).clamp(0.0, 1.0)
    };
    Score {
        value: q6(value),
        confidence: Confidence::High,
        rationale: format!(
            "{duration_ms}ms against {} ideal window [{}, {}]ms",
            platform.id, platform.ideal_min_ms, platform.ideal_max_ms
        ),
    }
}

/// Stage 5: visual support. `None` = no track supplied (neutral, Low). `Some(&[])` = a
/// track exists but nothing falls in this window (neutral, Low). Otherwise four weighted
/// signals: detector confidence, tracking-gap coverage, shot-boundary variety and
/// tracked-centre motion (shot-boundary jumps excluded from motion).
pub fn score_visual_support(
    points_in_window: Option<&[&VisualPoint]>,
    profile: &VisualSupportProfile,
) -> Score {
    let points = match points_in_window {
        None => {
            return Score {
                value: 0.5,
                confidence: Confidence::Low,
                rationale: "no reframe track supplied; scored neutrally pending that data".into(),
            }
        }
        Some([]) => {
            return Score {
                value: 0.5,
                confidence: Confidence::Low,
                rationale: "a reframe track exists but no sampled point falls within this \
                            window; scored neutrally"
                    .into(),
            }
        }
        Some(p) => p,
    };
    let count = points.len() as f64;
    let avg_confidence = points.iter().map(|p| p.confidence).sum::<f64>() / count;
    let gap_fraction = points.iter().filter(|p| p.gap).count() as f64 / count;
    let coverage = (1.0 - gap_fraction).clamp(0.0, 1.0);
    let shot_boundaries = points.iter().filter(|p| p.shot_boundary).count();
    let window_ms = points
        .last()
        .zip(points.first())
        .map(|(l, f)| l.source_ms.saturating_sub(f.source_ms) as i64)
        .unwrap_or(0)
        .max(1);
    let expected =
        (window_ms as f64 / profile.variety_target_shot_interval_ms.max(1) as f64).max(1.0);
    let variety = (shot_boundaries as f64 / expected).clamp(0.0, 1.0);
    let (mut total_step, mut steps) = (0.0, 0usize);
    for pair in points.windows(2) {
        if pair[1].shot_boundary {
            continue;
        }
        let (dx, dy) = (
            pair[1].center_x - pair[0].center_x,
            pair[1].center_y - pair[0].center_y,
        );
        total_step += (dx * dx + dy * dy).sqrt();
        steps += 1;
    }
    let avg_step = if steps > 0 {
        total_step / steps as f64
    } else {
        0.0
    };
    let motion = (avg_step / profile.motion_target_avg_step.max(1e-9)).clamp(0.0, 1.0);
    let total_weight = profile.confidence_weight
        + profile.coverage_weight
        + profile.variety_weight
        + profile.motion_weight;
    let value = if total_weight <= 0.0 {
        0.5
    } else {
        ((avg_confidence * profile.confidence_weight
            + coverage * profile.coverage_weight
            + variety * profile.variety_weight
            + motion * profile.motion_weight)
            / total_weight)
            .clamp(0.0, 1.0)
    };
    Score {
        value: q6(value),
        confidence: if points.len() >= profile.min_points_for_high_confidence {
            Confidence::High
        } else {
            Confidence::Medium
        },
        rationale: format!(
            "{value:.2}: subject tracked across {} sampled point(s), avg detector confidence \
             {avg_confidence:.2}, {:.0}% tracking gap, {shot_boundaries} shot boundary(ies) \
             ({variety:.2} variety), avg centre motion {avg_step:.4} ({motion:.2} motion)",
            points.len(),
            gap_fraction * 100.0,
        ),
    }
}

/// Stage 6a: platform fit. High confidence when output aspects are declared; vertical
/// (9:16) delivery scores 1.0, other targets 0.6; none declared is neutral, Low.
pub fn score_platform_fit(target_aspects: &[String]) -> Score {
    if target_aspects.is_empty() {
        return Score {
            value: 0.5,
            confidence: Confidence::Low,
            rationale: "no output targets declared; platform fit cannot be determined".into(),
        };
    }
    let vertical = target_aspects.iter().any(|a| a == "9:16");
    Score {
        value: if vertical { 1.0 } else { 0.6 },
        confidence: Confidence::High,
        rationale: if vertical {
            "a 9:16 output target is declared".into()
        } else {
            "output target(s) declared but none are 9:16".into()
        },
    }
}

/// Stage 6b: brand fit, a banned-term check; an empty list is neutral at Low.
pub fn score_brand_fit(text: &str, profile: &ShortsProfile) -> Score {
    if profile.brand_banned_terms.is_empty() {
        return Score {
            value: 0.5,
            confidence: Confidence::Low,
            rationale: "no brand banned-term list configured; brand fit cannot be verified".into(),
        };
    }
    let hits = contains_cue(&text.to_lowercase(), &profile.brand_banned_terms);
    if hits.is_empty() {
        Score {
            value: 1.0,
            confidence: Confidence::High,
            rationale: "no configured banned terms present".into(),
        }
    } else {
        Score {
            value: 0.0,
            confidence: Confidence::High,
            rationale: format!("matched banned term(s): {}", hits.join(", ")),
        }
    }
}

pub fn composite_score(b: &ScoreBreakdown, w: &crate::config::ScoreWeights) -> f64 {
    let total = w.hook
        + w.payoff
        + w.proof
        + w.value
        + w.duration_fit
        + w.visual_support
        + w.platform_fit
        + w.brand_fit;
    if total <= 0.0 {
        return 0.0;
    }
    q6((b.hook.value * w.hook
        + b.payoff.value * w.payoff
        + b.proof.value * w.proof
        + b.value.value * w.value
        + b.duration_fit.value * w.duration_fit
        + b.visual_support.value * w.visual_support
        + b.platform_fit.value * w.platform_fit
        + b.brand_fit.value * w.brand_fit)
        / total)
}

/// Token-set Jaccard similarity (lower-cased, whitespace-split).
pub fn jaccard_similarity(left: &str, right: &str) -> f64 {
    let set = |s: &str| -> HashSet<String> {
        s.to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect()
    };
    let (l, r) = (set(left), set(right));
    if l.is_empty() && r.is_empty() {
        return 1.0;
    }
    let union = l.union(&r).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        l.intersection(&r).count() as f64 / union
    }
}

/// Stage 7: diversity clustering. `texts` must already be sorted best-first. Each text
/// joins the first cluster whose representative is similar enough; selection then takes
/// one from each cluster in turn so near-paraphrases cannot fill every slot. Returns
/// (cluster index per input, selected input indices).
pub fn cluster_and_select(
    texts: &[&str],
    threshold: f64,
    count: usize,
) -> (Vec<usize>, Vec<usize>) {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut cluster_of = vec![0usize; texts.len()];
    for (index, text) in texts.iter().enumerate() {
        let joined = clusters
            .iter()
            .position(|c| jaccard_similarity(texts[c[0]], text) >= threshold);
        match joined {
            Some(ci) => {
                clusters[ci].push(index);
                cluster_of[index] = ci;
            }
            None => {
                cluster_of[index] = clusters.len();
                clusters.push(vec![index]);
            }
        }
    }
    let mut selected = Vec::new();
    let mut cursors = vec![0usize; clusters.len()];
    loop {
        let mut progressed = false;
        for (ci, cluster) in clusters.iter().enumerate() {
            if selected.len() >= count {
                break;
            }
            if cursors[ci] < cluster.len() {
                selected.push(cluster[cursors[ci]]);
                cursors[ci] += 1;
                progressed = true;
            }
        }
        if selected.len() >= count || !progressed {
            break;
        }
    }
    (cluster_of, selected)
}
