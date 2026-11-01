//! Retakes. The cut engine finds earlier attempts that a later take repeats and
//! keeps the last one; here that decision is completed:
//!
//! * the kept take must be COMPLETE: a later take that is cut off mid-sentence
//!   loses to an earlier complete one, and chains of takes keep the last
//!   complete take;
//! * spoken retake cues ("let me start over", "take two") remove the attempt
//!   that precedes them, whether or not the restart repeats its words.

use std::collections::BTreeMap;

use crate::cuts::{
    self_talk, take_tier, Decision, RemovalReason, RuleConfig, Tier as CutTier, Verdict,
};
use rightkit_transcript::{EndSource, Word};
use serde::{Deserialize, Serialize};

use crate::editorial::config::{EditorialConfig, RetakePolicy};
use crate::editorial::item::{word_draft, Draft, ItemKind, TakeRef, Tier};
use crate::editorial::util::{
    dice, ends_interrupted, ends_sentence, ends_soft, first_word_at, is_dangling, is_hesitation,
    join, phrase_at, phrase_words, round2, Wd,
};

/// One take of a line, scored and ranked against its siblings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeInfo {
    /// Chronological position within the group.
    pub index: usize,
    /// The retake item that removes this take (accepting it drops the take).
    pub item_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Transcript word range (end exclusive), including trailing debris.
    pub word_range: [usize; 2],
    pub text: String,
    pub complete: bool,
    /// Component scores 0..=1 (completeness, fluency, ending, pace, energy).
    pub scores: BTreeMap<String, f64>,
    /// Weighted total (plus the recency bonus).
    pub total: f64,
    /// 1 = best.
    pub rank: usize,
    /// The take the default edit keeps.
    pub chosen: bool,
    pub explanation: Vec<String>,
}

/// Several takes of the same line. The default keeps the best COMPLETE take;
/// `Review::pick_take` keeps another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeGroup {
    pub id: String,
    pub chosen: usize,
    pub takes: Vec<TakeInfo>,
}

pub(crate) struct RetakeOut {
    pub drafts: Vec<Draft>,
    /// Words of the take each group keeps (marked retakes never remove them).
    pub protected: Vec<bool>,
    pub groups: Vec<TakeGroup>,
}

struct Hit {
    from: usize,
    to: usize,
    sim: f64,
}

struct Take {
    start: usize,
    /// End of the take proper (exclusive); `end` includes trailing debris.
    core_end: usize,
    end: usize,
}

fn token_count(words: &[Wd], a: usize, b: usize) -> usize {
    (a..b)
        .filter(|&i| !words[i].norm.is_empty() && !is_hesitation(&words[i].norm))
        .count()
}

/// Whether the transcript carries sentence punctuation at all (an ASR that
/// punctuates marks a finished take with a terminal mark).
pub(crate) fn is_punctuated(words: &[Wd]) -> bool {
    let n = words.iter().filter(|w| ends_sentence(&w.text)).count();
    n > 0 && words.len() / n.max(1) <= 40
}

fn complete(
    words: &[Wd],
    a: usize,
    b: usize,
    ref_tokens: usize,
    punctuated: bool,
    p: &RetakePolicy,
) -> bool {
    let Some(last) = (a..b)
        .rev()
        .find(|&i| !words[i].norm.is_empty() && !is_hesitation(&words[i].norm))
    else {
        return false;
    };
    let t = &words[last].text;
    if ends_sentence(t) {
        return true;
    }
    if ends_interrupted(t) || ends_soft(t) || is_dangling(&words[last].norm) {
        return false;
    }
    if punctuated {
        return false;
    }
    token_count(words, a, b) as f32 >= p.min_take_ratio * ref_tokens as f32
}

fn hits_from(words: &[Wd], decisions: &[Decision]) -> Vec<Hit> {
    let mut v: Vec<Hit> = decisions
        .iter()
        .filter(|d| d.kind == RemovalReason::Retake)
        .map(|d| {
            let sim = d
                .detail
                .split("similarity ")
                .nth(1)
                .and_then(|s| s.split(|c: char| !(c.is_ascii_digit() || c == '.')).next())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.8);
            Hit {
                from: first_word_at(words, d.start_ms),
                to: first_word_at(words, d.end_ms),
                sim,
            }
        })
        .filter(|h| h.from < h.to && h.to < words.len())
        .collect();
    v.sort_by_key(|h| h.from);
    v
}

/// Trailing words after the last sentence end of an attempt are debris
/// ("um, sorry, let me..."), not part of the take.
fn core_end_of(words: &[Wd], from: usize, to: usize) -> usize {
    match (from..to).rev().find(|&j| ends_sentence(&words[j].text)) {
        Some(j) if j + 1 < to && to - (j + 1) <= 8 => j + 1,
        _ => to,
    }
}

fn extent_end(words: &[Wd], from: usize, ref_tokens: usize, p: &RetakePolicy) -> usize {
    let n = words.len();
    let mut count = 0;
    for i in from..n {
        if !words[i].norm.is_empty() && !is_hesitation(&words[i].norm) {
            count += 1;
        }
        if i + 1 == n {
            return n;
        }
        let gap = words[i + 1].start - words[i].end;
        if gap >= p.take_end_pause_ms
            || (ends_sentence(&words[i].text) && count as f32 >= 0.7 * ref_tokens as f32)
        {
            return i + 1;
        }
    }
    n
}

pub(crate) fn detect(
    words: &[Wd],
    decisions: &[Decision],
    envelope: Option<&[f32]>,
    cfg: &EditorialConfig,
    punctuated: bool,
) -> RetakeOut {
    let policy = &cfg.retake;
    let mut out = RetakeOut {
        drafts: Vec::new(),
        protected: vec![false; words.len()],
        groups: Vec::new(),
    };
    let hits = hits_from(words, decisions);
    let tw = rule_words(words);
    let mut i = 0;
    while i < hits.len() {
        let mut j = i;
        while j + 1 < hits.len() && hits[j + 1].from == hits[j].to {
            j += 1;
        }
        chain(
            words,
            &tw,
            &hits[i..=j],
            envelope,
            policy,
            punctuated,
            &mut out,
        );
        i = j + 1;
    }
    out
}

/// The rule layer reads transcript words; `Wd` carries the same timings and text.
fn rule_words(words: &[Wd]) -> Vec<Word> {
    words
        .iter()
        .map(|w| Word {
            text: w.text.clone(),
            start_ms: w.start.max(0) as u64,
            end_ms: w.end.max(0) as u64,
            confidence: w.conf,
            end_source: EndSource::Model,
        })
        .collect()
}

/// Raw measurements of one take.
struct Raw {
    complete: bool,
    fluency: f64,
    ending: f64,
    wps: f64,
    energy: Option<f64>,
    hesitations: usize,
    words: usize,
}

fn raw_score(
    words: &[Wd],
    t: &Take,
    reference: usize,
    envelope: Option<&[f32]>,
    punctuated: bool,
    policy: &RetakePolicy,
) -> Raw {
    let (a, b) = (t.start, t.core_end);
    let all: Vec<usize> = (a..b).filter(|&i| !words[i].norm.is_empty()).collect();
    let n = all.len().max(1);
    let hes = all
        .iter()
        .filter(|&&i| is_hesitation(&words[i].norm))
        .count();
    let repeats = all
        .windows(2)
        .filter(|w| words[w[0]].norm == words[w[1]].norm)
        .count();
    let frags = all
        .iter()
        .filter(|&&i| ends_interrupted(&words[i].text) && i + 1 < b)
        .count();
    let bad = hes + repeats + frags;
    let fluency = 1.0 - ((bad as f64 / n as f64) * 3.0).min(1.0);
    let last = all
        .iter()
        .rev()
        .find(|&&i| !is_hesitation(&words[i].norm))
        .copied();
    let ending = match last {
        None => 0.0,
        Some(l) => {
            let txt = &words[l].text;
            if ends_sentence(txt) {
                1.0
            } else if ends_interrupted(txt) || is_dangling(&words[l].norm) {
                0.0
            } else if ends_soft(txt) {
                0.3
            } else {
                0.6
            }
        }
    };
    let span_ms = match (all.first(), all.last()) {
        (Some(&f), Some(&l)) => (words[l].end - words[f].start).max(1) as f64,
        _ => 1.0,
    };
    let energy = envelope.filter(|e| !e.is_empty()).and_then(|e| {
        let (mut sum, mut cnt) = (0.0f64, 0usize);
        for &i in &all {
            let fa = (words[i].start.max(0) / 10) as usize;
            let fb = (words[i].end.max(0) / 10) as usize;
            for v in e.iter().take(fb.min(e.len())).skip(fa) {
                sum += *v as f64;
                cnt += 1;
            }
        }
        (cnt > 0).then(|| sum / cnt as f64)
    });
    Raw {
        complete: complete(words, a, b, reference, punctuated, policy),
        fluency,
        ending,
        wps: token_count(words, a, b) as f64 / (span_ms / 1000.0),
        energy,
        hesitations: hes,
        words: n,
    }
}

fn chain(
    words: &[Wd],
    tw: &[Word],
    hits: &[Hit],
    envelope: Option<&[f32]>,
    policy: &RetakePolicy,
    punctuated: bool,
    out: &mut RetakeOut,
) {
    let mut takes: Vec<Take> = hits
        .iter()
        .map(|h| Take {
            start: h.from,
            core_end: core_end_of(words, h.from, h.to),
            end: h.to,
        })
        .collect();
    let last_hit = hits.last().expect("chain has a hit");
    let ref_len = token_count(words, last_hit.from, last_hit.to);
    let ext = extent_end(words, last_hit.to, ref_len, policy);
    takes.push(Take {
        start: last_hit.to,
        core_end: ext,
        end: ext,
    });
    let k = takes.len();
    let lens: Vec<usize> = takes
        .iter()
        .map(|t| token_count(words, t.start, t.core_end))
        .collect();
    let raws: Vec<Raw> = (0..k)
        .map(|i| {
            // Judge each take against the take it was replayed by (or replays).
            let reference = if i + 1 < k { lens[i + 1] } else { lens[i - 1] };
            raw_score(
                words,
                &takes[i],
                reference.max(lens[i]),
                envelope,
                punctuated,
                policy,
            )
        })
        .collect();
    let mut wps: Vec<f64> = raws.iter().map(|r| r.wps).collect();
    wps.sort_by(|a, b| a.total_cmp(b));
    let median = wps[wps.len() / 2].max(1e-6);
    let max_energy = raws.iter().filter_map(|r| r.energy).fold(0.0f64, f64::max);
    let w = &policy.weights;
    let mut totals = Vec::with_capacity(k);
    let mut score_maps = Vec::with_capacity(k);
    for (i, r) in raws.iter().enumerate() {
        let comp = if r.complete { 1.0 } else { 0.0 };
        let pace = 1.0 - ((r.wps - median).abs() / median).min(1.0);
        let mut parts: Vec<(&str, f64, f64)> = vec![
            ("completeness", comp, w.completeness as f64),
            ("fluency", r.fluency, w.fluency as f64),
            ("ending", r.ending, w.ending as f64),
            ("pace", pace, w.pace as f64),
        ];
        if let (Some(e), true) = (r.energy, max_energy > 0.0) {
            parts.push(("energy", e / max_energy, w.energy as f64));
        }
        let wsum: f64 = parts.iter().map(|p| p.2).sum::<f64>().max(1e-9);
        let base: f64 = parts.iter().map(|p| p.1 * p.2).sum::<f64>() / wsum;
        let bonus = if k > 1 {
            policy.recency_bonus as f64 * i as f64 / (k - 1) as f64
        } else {
            0.0
        };
        let mut m: BTreeMap<String, f64> = parts
            .iter()
            .map(|p| (p.0.to_string(), round2(p.1)))
            .collect();
        m.insert("wps".into(), round2(r.wps));
        totals.push(base + bonus);
        score_maps.push(m);
    }
    let by_total = |a: &usize, b: &usize| totals[*a].total_cmp(&totals[*b]).then(a.cmp(b));
    let winner = if policy.prefer_complete {
        (0..k)
            .filter(|&i| raws[i].complete)
            .max_by(by_total)
            .or_else(|| (0..k).max_by(by_total))
            .unwrap_or(k - 1)
    } else {
        k - 1
    };
    let mut order: Vec<usize> = (0..k).collect();
    order.sort_by(|&a, &b| totals[b].total_cmp(&totals[a]).then(b.cmp(&a)));
    let rank_of = |i: usize| order.iter().position(|&o| o == i).unwrap_or(0) + 1;
    let kept_text = join(
        words,
        takes[winner].start,
        takes[winner].core_end.max(takes[winner].start + 1),
    );
    for slot in &mut out.protected[takes[winner].start..takes[winner].core_end] {
        *slot = true;
    }
    let group_id = format!("take-group-{:03}", out.groups.len() + 1);
    let mut infos = Vec::with_capacity(k);
    for i in 0..k {
        let r = &raws[i];
        let mut why: Vec<String> = vec![
            if r.complete {
                "complete take".to_string()
            } else {
                "cut off mid-sentence".to_string()
            },
            format!("{} hesitation(s) in {} words", r.hesitations, r.words),
            if r.ending >= 1.0 {
                "ends cleanly".to_string()
            } else if r.ending >= 0.5 {
                "ends without a full stop".to_string()
            } else {
                "ends dangling or interrupted".to_string()
            },
            format!("pace {:.1} words/s (median {:.1})", r.wps, median),
        ];
        if let (Some(e), true) = (r.energy, max_energy > 0.0) {
            why.push(format!(
                "energy {:.0}% of the loudest take",
                100.0 * e / max_energy
            ));
        }
        let t = &takes[i];
        infos.push(TakeInfo {
            index: i,
            item_id: String::new(),
            start_ms: words[t.start].start,
            end_ms: words[t.end.max(t.start + 1) - 1].end,
            word_range: [t.start, t.end],
            text: join(words, t.start, t.core_end.max(t.start + 1)),
            complete: r.complete,
            scores: score_maps[i].clone(),
            total: round2(totals[i]),
            rank: rank_of(i),
            chosen: i == winner,
            explanation: why,
        });
        let (first, end) = (t.start, t.end);
        if first >= end {
            continue;
        }
        let sim = hits[i.min(hits.len() - 1)].sim;
        let is_winner = i == winner;
        let reason = if is_winner {
            format!(
                "alternative take (rank {} of {k}); choose to keep it instead",
                rank_of(i)
            )
        } else if i > winner {
            "later take is weaker than the best complete take; best take kept".to_string()
        } else {
            format!(
                "earlier attempt repeats the later take (similarity {sim:.2}); best complete take kept"
            )
        };
        let confidence = if is_winner {
            0.4
        } else if i > winner {
            0.75
        } else {
            (0.6 + 0.4 * sim) as f32
        };
        // A later take that stops mid-phrase is a suggestion, never an automatic cut.
        let broken_later = i > winner && take_tier(tw, t.start..t.core_end) == CutTier::Suggest;
        let reason = if broken_later {
            "later take stops mid-phrase: suggested for review, not cut".to_string()
        } else {
            reason
        };
        let tier = if is_winner || broken_later {
            Tier::Suggest
        } else {
            Tier::Auto
        };
        let mut d = word_draft(
            words,
            ItemKind::Retake,
            first,
            end,
            tier,
            confidence,
            reason,
        );
        d.take = Some(TakeRef {
            group_id: group_id.clone(),
            index: i,
        });
        d.evidence.kept_text = Some(kept_text.clone());
        d.evidence.signals.insert("similarity".into(), round2(sim));
        d.evidence
            .signals
            .insert("take_score".into(), round2(totals[i]));
        d.evidence
            .signals
            .insert("take_rank".into(), rank_of(i) as f64);
        d.evidence
            .signals
            .insert("take_complete".into(), if r.complete { 1.0 } else { 0.0 });
        if i > winner {
            d.evidence
                .notes
                .push("the cut engine alone would have kept this later take".into());
        }
        out.drafts.push(d);
    }
    out.groups.push(TakeGroup {
        id: group_id,
        chosen: winner,
        takes: infos,
    });
}

/// Spoken retake cues: remove the cue and the attempt it abandons.
pub(crate) fn marked(words: &[Wd], policy: &RetakePolicy, protected: &[bool]) -> Vec<Draft> {
    if !policy.marked_retakes {
        return Vec::new();
    }
    let mut cues: Vec<Vec<String>> = policy
        .cues
        .iter()
        .map(|c| phrase_words(c))
        .filter(|c| !c.is_empty())
        .collect();
    cues.sort_by_key(|c| std::cmp::Reverse(c.len()));
    let idx: Vec<usize> = (0..words.len())
        .filter(|&i| !words[i].norm.is_empty())
        .collect();
    let m = idx.len();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < m {
        let Some(len) = cues
            .iter()
            .find(|c| phrase_at(words, &idx, pos, c))
            .map(|c| c.len())
        else {
            pos += 1;
            continue;
        };
        let (c0, c1) = (pos, pos + len);
        let sep = |a: usize, b: usize| words[idx[b]].start - words[idx[a]].end;
        // Attempt: back to the previous sentence start or long pause.
        let walk = |mut s: usize| {
            while s > 0
                && c0 - s < policy.cue_max_back_words
                && !ends_sentence(&words[idx[s - 1]].text)
                && sep(s - 1, s) < policy.cue_boundary_pause_ms
            {
                s -= 1;
            }
            s
        };
        let mut s = walk(c0);
        if s == c0 && c0 > 0 {
            // The cue opens its own sentence ("... look at- Sorry. Take two."):
            // the attempt is the sentence before it.
            s = walk(c0 - 1);
        }
        let first = idx[s];
        let end = idx[c1 - 1] + 1;
        if (first..end).any(|w| protected[w]) {
            pos = c1;
            continue;
        }
        let attempt: Vec<&str> = (s..c0).map(|k| words[idx[k]].norm.as_str()).collect();
        let lr = attempt.len().min(m - c1);
        let restart: Vec<&str> = (c1..c1 + lr).map(|k| words[idx[k]].norm.as_str()).collect();
        let sim = round2(dice(&attempt, &restart) as f64);
        let similar = !attempt.is_empty() && (sim >= 0.5 || attempt.first() == restart.first());
        let (tier, conf) = if attempt.is_empty() {
            (Tier::Auto, 0.8)
        } else if similar {
            (Tier::Auto, 0.9)
        } else {
            (Tier::Suggest, 0.55)
        };
        let cue = join(words, idx[c0], end);
        let mut d = word_draft(
            words,
            ItemKind::MarkedRetake,
            first,
            end,
            tier,
            conf,
            format!("spoken retake cue \"{cue}\""),
        );
        if c1 < m {
            let kept_end = idx[(c1 + lr.max(1) - 1).min(m - 1)] + 1;
            d.evidence.kept_text = Some(join(words, idx[c1], kept_end));
        }
        d.evidence.signals.insert("similarity".into(), sim);
        d.evidence
            .signals
            .insert("attempt_words".into(), attempt.len() as f64);
        out.push(d);
        pos = c1;
    }
    // Self-talk restarts: an abandoned attempt plus a strong cue is removed. Spans the
    // cue pass already claimed are skipped so one attempt yields one item.
    let tw = rule_words(words);
    for hit in self_talk(&tw, &RuleConfig::default()) {
        if hit.verdict != Verdict::Cut || (hit.first..hit.end).any(|w| protected[w]) {
            continue;
        }
        let overlaps = out.iter().any(|d| {
            d.evidence
                .word_range
                .is_some_and(|[a, b]| a < hit.end && hit.first < b)
        });
        if overlaps {
            continue;
        }
        out.push(word_draft(
            words,
            ItemKind::MarkedRetake,
            hit.first,
            hit.end,
            Tier::Auto,
            0.85,
            hit.reason,
        ));
    }
    out
}
