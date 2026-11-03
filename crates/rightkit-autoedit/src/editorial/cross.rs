//! Conservative cross-file take ranking using sentence boundaries, repeated
//! wording, completeness & fluency. Ambiguous shared fragments are preserved.

use rightkit_transcript::Transcript;
use serde::{Deserialize, Serialize};

use crate::editorial::util::{
    ends_interrupted, ends_sentence, is_dangling, is_hesitation, normalize,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossFileRetake {
    pub loser_source: String,
    pub kept_source: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub words: usize,
    pub text: String,
    pub kept_text: String,
    pub word_range: [usize; 2],
    pub kept_start_ms: i64,
    pub kept_end_ms: i64,
    pub take_complete: bool,
    pub kept_complete: bool,
    pub take_fluency: f64,
    pub kept_fluency: f64,
}

struct Take {
    source: usize,
    first: usize,
    end: usize,
    tokens: Vec<String>,
    terminal: bool,
    interrupted: bool,
    cutoff: bool,
    /// The take is cut off at the file end and its last word lasts at most
    /// [`CLIPPED_WORD_MS`]: recognition of a word the recording ends inside.
    clipped_tail: bool,
    fluency: f64,
}

/// Longest final word that still counts as a fragment of a word the file ends inside.
/// Recognition turns such a fragment into some short unrelated word.
const CLIPPED_WORD_MS: u64 = 200;

fn takes(source: usize, t: &Transcript) -> Vec<Take> {
    let mut result = Vec::new();
    let mut first = 0;
    for (i, w) in t.words.iter().enumerate() {
        let boundary = t.words.get(i + 1).is_none_or(|next| {
            // TDT word ends can include the following pause, & the next word
            // may start before VAD's onset. Measure the actual intervening VAD
            // gap instead of using either model word edge as its end.
            let vad_gap = t.vad.windows(2).any(|pair| {
                pair[0].end_ms >= w.start_ms
                    && pair[0].end_ms <= next.start_ms
                    && pair[1].start_ms >= next.start_ms
                    && pair[1].start_ms <= next.end_ms
                    && pair[1].start_ms.saturating_sub(pair[0].end_ms) >= 650
            });
            vad_gap || next.start_ms.saturating_sub(w.end_ms) >= 650
        });
        if !ends_sentence(&w.text) && !boundary {
            continue;
        }
        let all: Vec<_> = t.words[first..=i]
            .iter()
            .map(|w| normalize(&w.text))
            .collect();
        let hesitations = all.iter().filter(|w| is_hesitation(w)).count();
        let repeated = all.windows(2).filter(|pair| pair[0] == pair[1]).count();
        let tokens: Vec<_> = all
            .into_iter()
            .filter(|w| !w.is_empty() && !is_hesitation(w))
            .collect();
        // A VAD region reaching the physical file boundary is positive
        // truncation evidence even if ASR invents terminal punctuation.
        let cutoff = i + 1 == t.words.len()
            // Two encoder frames can separate the final word timestamp from
            // physical EOF; VAD reaching EOF remains required evidence.
            && t.duration_ms.saturating_sub(w.end_ms) <= 160
            && t.vad
                .last()
                .is_some_and(|v| t.duration_ms.saturating_sub(v.end_ms) <= 80);
        let interrupted =
            ends_interrupted(&w.text) || tokens.last().is_some_and(|w| is_dangling(w));
        result.push(Take {
            source,
            first,
            end: i + 1,
            tokens,
            terminal: ends_sentence(&w.text),
            interrupted,
            cutoff,
            clipped_tail: cutoff && w.end_ms.saturating_sub(w.start_ms) <= CLIPPED_WORD_MS,
            fluency: 1.0 - ((hesitations + repeated) as f64 / (i + 1 - first) as f64).min(1.0),
        });
        first = i + 1;
    }
    result
}

fn complete(t: &Take, reference: usize) -> bool {
    !t.interrupted
        && (t.terminal || t.tokens.len() == reference)
        && (!t.cutoff || t.tokens.len() == reference)
}

fn root(parents: &[usize], mut i: usize) -> usize {
    while parents[i] != i {
        i = parents[i];
    }
    i
}

/// Groups matching whole takes, then selects one winner for every group (even
/// with three or more files). Only identical wording or a cut-off prefix of a
/// longer take is removed; shared fragments & different arguments survive.
pub fn cross_file_retakes(
    sources: &[(String, Transcript)],
    min_words: usize,
) -> Vec<CrossFileRetake> {
    let all: Vec<_> = sources
        .iter()
        .enumerate()
        .flat_map(|(i, (_, t))| takes(i, t))
        .collect();
    let mut parents: Vec<_> = (0..all.len()).collect();
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate().skip(i + 1) {
            if a.source == b.source {
                continue;
            }
            let shared = a
                .tokens
                .iter()
                .zip(&b.tokens)
                .take_while(|(x, y)| x == y)
                .count();
            let shorter = a.tokens.len().min(b.tokens.len());
            let reference = a.tokens.len().max(b.tokens.len());
            // The shorter take must be a prefix of the longer one. A take the file cuts
            // off inside a word ends in a fragment that matches nothing; that one
            // token is not held against it.
            let clipped = (a.tokens.len() < b.tokens.len() && a.clipped_tail)
                || (b.tokens.len() < a.tokens.len() && b.clipped_tail);
            let prefix = shared == shorter || (clipped && shared + 1 == shorter);
            if shared < min_words.max(3) || !prefix {
                continue;
            }
            // Complete sentences with different continuations are distinct
            // arguments; truncation must be evidenced by an unfinished ending.
            if a.tokens != b.tokens
                && ((a.tokens.len() < b.tokens.len() && a.terminal && !a.interrupted && !a.cutoff)
                    || (b.tokens.len() < a.tokens.len()
                        && b.terminal
                        && !b.interrupted
                        && !b.cutoff)
                    || (!complete(a, reference) && !complete(b, reference)))
            {
                continue;
            }
            let (ri, rj) = (root(&parents, i), root(&parents, j));
            parents[ri] = rj;
        }
    }
    let mut hits = Vec::new();
    for group in 0..all.len() {
        if root(&parents, group) != group {
            continue;
        }
        let members: Vec<_> = (0..all.len())
            .filter(|&i| root(&parents, i) == group)
            .collect();
        if members.len() < 2 {
            continue;
        }
        let reference = members.iter().map(|&i| all[i].tokens.len()).max().unwrap();
        let winner = *members
            .iter()
            .max_by(|&&i, &&j| {
                let (a, b) = (&all[i], &all[j]);
                complete(a, reference)
                    .cmp(&complete(b, reference))
                    .then_with(|| a.fluency.total_cmp(&b.fluency))
                    .then_with(|| a.tokens.len().cmp(&b.tokens.len()))
                    .then_with(|| a.source.cmp(&b.source))
            })
            .unwrap();
        let kept = &all[winner];
        let kt = &sources[kept.source].1;
        // Leave the winner's same-file alternatives to its editorial review.
        // Cross-file grouping must not turn local Suggest items into Auto cuts.
        for loser in members
            .into_iter()
            .filter(|&i| all[i].source != kept.source)
        {
            let take = &all[loser];
            let t = &sources[take.source].1;
            let text = |t: &Transcript, a: usize, b: usize| {
                t.words[a..b]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            hits.push(CrossFileRetake {
                loser_source: sources[take.source].0.clone(),
                kept_source: sources[kept.source].0.clone(),
                start_ms: t.words[take.first].start_ms as i64,
                end_ms: t.words[take.end - 1].end_ms as i64,
                words: take.tokens.len(),
                text: text(t, take.first, take.end),
                kept_text: text(kt, kept.first, kept.end),
                word_range: [take.first, take.end],
                kept_start_ms: kt.words[kept.first].start_ms as i64,
                kept_end_ms: kt.words[kept.end - 1].end_ms as i64,
                take_complete: complete(take, reference),
                kept_complete: complete(kept, reference),
                take_fluency: take.fluency,
                kept_fluency: kept.fluency,
            });
        }
    }
    hits.sort_by(|a, b| (&a.loser_source, a.start_ms).cmp(&(&b.loser_source, b.start_ms)));
    hits
}
