//! LLM-first ranking with a heuristic fallback.
//!
//! [`propose_shorts_ranked`] sends transcript sentences (ids, no times) to a
//! [`MomentProvider`], validates every returned moment against recogniser word
//! boundaries, runs the validated windows through the same heuristic stage scorer as
//! the fallback, and blends the model score into the composite. Without a provider (no
//! key) or when the provider fails or returns nothing usable, the 9-stage heuristic
//! pipeline runs and `ranking.fallback_reason` says why.

use std::collections::BTreeMap;

use crate::budget::{plan_requests, BudgetAction};
use crate::config::{PlatformWindow, ShortsProfile};
use crate::error::ShortsError;
use crate::llm::{
    fixed_prompt_chars, sentence_chars, LlmMoment, MomentProvider, MomentRequest, SentenceItem,
};
use crate::propose::*;
use crate::score::norm_word;

/// The request for `sentences` under the profile's limits (no sentences: the shell whose
/// fixed prompt size the budget measures).
fn request_for(
    sentences: Vec<SentenceItem>,
    profile: &ShortsProfile,
    primary: &PlatformWindow,
) -> MomentRequest {
    MomentRequest {
        sentences,
        max_moments: profile.llm.max_moments_per_request,
        min_duration_ms: primary.min_duration_ms,
        ideal_min_ms: primary.ideal_min_ms,
        ideal_max_ms: primary.ideal_max_ms,
        max_duration_ms: primary.max_duration_ms,
    }
}

const MAX_REPORTED_LLM_REJECTIONS: usize = 50;

fn tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(norm_word)
        .filter(|t| !t.is_empty())
        .collect()
}

/// `needle` appears as a contiguous run of tokens inside `hay`.
fn contains_run(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && needle.len() <= hay.len()
        && hay.windows(needle.len()).any(|w| w == needle)
}

fn clean_title(text: &str, max: usize) -> String {
    let question = text.trim_end().ends_with('?');
    let t = text
        .trim()
        .trim_end_matches(['.', ',', ';', ':', '!', '?', '\u{2026}'])
        .trim();
    let reserve = usize::from(question);
    let mut out = String::new();
    for w in t.split_whitespace() {
        let next = if out.is_empty() {
            w.to_string()
        } else {
            format!("{out} {w}")
        };
        if next.chars().count() + reserve > max {
            break;
        }
        out = next;
    }
    let mut chars = out.chars();
    let mut title = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => return String::new(),
    };
    if question {
        title.push('?');
    }
    title
}

/// Ranks with the LLM when `provider` is `Ok`, else (or on any failure) with the
/// heuristic scorer. `Err(reason)` is why no provider is available.
pub fn propose_shorts_ranked(
    input: &ProposeInput<'_>,
    profile: &ShortsProfile,
    count: usize,
    provider: Result<&dyn MomentProvider, String>,
) -> Result<Proposals, ShortsError> {
    profile.validate()?;
    input.transcript.validate()?;
    if let Some(features) = input.audio {
        features.validate()?;
    }
    let provider = match provider {
        Ok(p) => p,
        Err(reason) => {
            return propose_heuristic(input, profile, count, RankingInfo::heuristic(Some(reason)))
        }
    };
    let tr = input.transcript;
    let words = &tr.words;
    let primary = profile.primary();
    let cx = Ctx {
        input,
        profile,
        sents: sentences(words, profile),
    };

    // Sentence list for the model: filler-only sentences are left out, ids are global.
    let items: Vec<(usize, SentenceItem)> = cx
        .sents
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let (f, l) = trimmed_range(*s, words, profile)?;
            let text = words[f..=l]
                .iter()
                .map(|w| w.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            Some((
                i,
                SentenceItem {
                    id: format!("s{:04}", i + 1),
                    start_ms: words[f].start_ms,
                    end_ms: words[l].end_ms,
                    text,
                },
            ))
        })
        .collect();

    let mut info = RankingInfo {
        method: "llm".into(),
        provider: Some(provider.name().to_string()),
        model: Some(provider.model().to_string()),
        fallback_reason: None,
        llm_moments_returned: 0,
        llm_moments_accepted: 0,
        llm_rejections: Vec::new(),
        prompt_budget: None,
    };
    let fallback = |mut info: RankingInfo, reason: String| {
        info.method = "heuristic".into();
        info.fallback_reason = Some(reason);
        propose_heuristic(input, profile, count, info)
    };

    // Token budget: pack sentences into requests that fit the prompt budget.
    let shell = request_for(Vec::new(), profile, primary);
    let line_chars: Vec<usize> = items.iter().map(|(_, it)| sentence_chars(it)).collect();
    let plan = plan_requests(
        &line_chars,
        fixed_prompt_chars(&shell),
        profile.llm.prompt_token_budget,
        profile.llm.max_sentences_per_request,
    );
    info.prompt_budget = Some(plan.report.clone());
    if plan.report.action == BudgetAction::Refused {
        let reason = format!(
            "prompt token budget refused the LLM request: {}",
            plan.report.note
        );
        return fallback(info, reason);
    }

    let id_to_sentence: BTreeMap<&str, usize> =
        items.iter().map(|(i, it)| (it.id.as_str(), *i)).collect();
    let mut by_window: BTreeMap<(usize, usize), Proposal> = BTreeMap::new();
    let mut rejected: Vec<RejectedWindow> = Vec::new();
    for range in &plan.requests {
        let part = &items[range.clone()];
        let request = request_for(
            part.iter().map(|(_, it)| it.clone()).collect(),
            profile,
            primary,
        );
        let moments = match provider.rank_moments(&request) {
            Ok(m) => m,
            Err(e) => {
                return fallback(info, format!("provider error: {e}"));
            }
        };
        let allowed: BTreeMap<&str, usize> = part
            .iter()
            .filter_map(|(i, it)| {
                id_to_sentence
                    .get(it.id.as_str())
                    .map(|_| (it.id.as_str(), *i))
            })
            .collect();
        for m in moments.iter().take(profile.llm.max_moments_per_request) {
            info.llm_moments_returned += 1;
            match validate_moment(&cx, m, &allowed, provider) {
                Ok(p) => {
                    info.llm_moments_accepted += 1;
                    let key = (p.first_word, p.last_word);
                    match by_window.get(&key) {
                        Some(old) if old.scores.composite >= p.scores.composite => {}
                        _ => {
                            by_window.insert(key, p);
                        }
                    }
                }
                Err(r) => {
                    if info.llm_rejections.len() < MAX_REPORTED_LLM_REJECTIONS {
                        info.llm_rejections.push(format!(
                            "{}..{}: {} ({})",
                            m.start_sentence, m.end_sentence, r.reason, r.stage
                        ));
                    }
                    if rejected.len() < profile.max_reported_rejections {
                        rejected.push(r);
                    }
                }
            }
        }
    }
    if by_window.is_empty() {
        let reason = format!(
            "the model returned no usable moments ({} returned, all rejected by word-boundary \
             or stage validation)",
            info.llm_moments_returned
        );
        return fallback(info, reason);
    }
    let windows_considered = info.llm_moments_returned;
    let rejected_total = info.llm_moments_returned - info.llm_moments_accepted;
    let (proposals, suppressed_overlap) = finish(by_window.into_values().collect(), profile, count);
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
        ranking: info,
        proposals,
    })
}

/// Resolves a moment's sentence ids to word edges and runs the heuristic gates and
/// stage scores on that window; merges the model score into the composite.
fn validate_moment(
    cx: &Ctx<'_>,
    m: &LlmMoment,
    allowed: &BTreeMap<&str, usize>,
    provider: &dyn MomentProvider,
) -> Result<Proposal, RejectedWindow> {
    let (profile, tr) = (cx.profile, cx.input.transcript);
    let words = &tr.words;
    let bad = |reason: String| RejectedWindow {
        start_ms: 0,
        end_ms: 0,
        stage: "llm_validation".into(),
        reason,
    };
    let lookup = |id: &str| {
        allowed
            .get(id)
            .copied()
            .ok_or_else(|| bad(format!("unknown sentence id {id:?}")))
    };
    let (si, sj) = (lookup(&m.start_sentence)?, lookup(&m.end_sentence)?);
    if si > sj {
        return Err(bad("end_sentence precedes start_sentence".into()));
    }
    if !m.score.is_finite() || !(0.0..=1.0).contains(&m.score) {
        return Err(bad(format!("score {} outside 0..=1", m.score)));
    }
    let (first, _) = trimmed_range(cx.sents[si], words, profile)
        .ok_or_else(|| bad("start sentence is only fillers".into()))?;
    let (_, last) = trimmed_range(cx.sents[sj], words, profile)
        .ok_or_else(|| bad("end sentence is only fillers".into()))?;
    if last >= words.len() || first > last || words[last].end_ms > tr.duration_ms {
        return Err(bad("moment is outside the transcript".into()));
    }
    for k in first..last {
        let pause = words[k + 1].start_ms.saturating_sub(words[k].end_ms) as i64;
        if pause > profile.windows.max_internal_pause_ms {
            return Err(bad(format!("moment spans a {pause}ms pause")));
        }
    }
    let mut p = score_window(cx, si, sj, first, last)?;

    let hook_tokens = tokens(&m.hook_text);
    let verified = contains_run(&tokens(&p.transcript), &hook_tokens);
    if verified {
        let title = clean_title(&m.hook_text, profile.title.max_chars);
        if !title.is_empty() {
            p.title = title;
        }
    }
    let heuristic = p.scores.composite;
    let blend = profile.llm.blend;
    p.scores.composite = (1.0 - blend) * heuristic + blend * m.score;
    p.rationale = format!(
        "{} | model score {:.2} blended at {:.2} -> {:.2}",
        p.rationale, m.score, blend, p.scores.composite
    );
    p.llm = Some(LlmEvidence {
        provider: provider.name().to_string(),
        model: provider.model().to_string(),
        score: m.score,
        reason: m.reason.clone(),
        hook_text: m.hook_text.clone(),
        hook_verified: verified,
        start_sentence: m.start_sentence.clone(),
        end_sentence: m.end_sentence.clone(),
        heuristic_composite: heuristic,
    });
    Ok(p)
}
