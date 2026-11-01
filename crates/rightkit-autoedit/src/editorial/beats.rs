//! Narrative beats from transcript structure: sentence boundaries, pauses and
//! cue words, scored per label (hook / setup / point / example / CTA). No model:
//! the cue lists below are the heuristic, and every beat reports the cues and
//! scores that produced its label.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::editorial::config::BeatConfig;
use crate::editorial::util::{ends_sentence, phrase_words, round2, Wd};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeatLabel {
    Hook,
    Setup,
    Point,
    Example,
    Cta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Beat {
    pub id: String,
    pub label: BeatLabel,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Transcript word range (end exclusive).
    pub word_range: [usize; 2],
    pub sentences: usize,
    pub text: String,
    /// 0.0..=1.0: margin of the winning label over the runner-up.
    pub confidence: f32,
    pub scores: BTreeMap<String, f64>,
    pub cues: Vec<String>,
    /// For hooks: specificity / promise / self-containment average, 0..=1.
    pub hook_score: Option<f32>,
}

struct Sentence {
    first: usize,
    end: usize,
    start_ms: i64,
    end_ms: i64,
    padded: String,
    words: usize,
    question: bool,
}

const GREETING: &[&str] = &[
    "welcome",
    "hey",
    "hi everyone",
    "hello",
    "what's up",
    "welcome back",
    "good morning",
];
const SETUP: &[&str] = &[
    "today",
    "in this video",
    "let's talk about",
    "let's dive",
    "i'm going to",
    "we're going to",
    "going to show",
    "going to talk",
    "first",
    "before we",
    "background",
    "the problem",
    "here's the thing",
    "so what is",
    "what is",
];
const EXAMPLE: &[&str] = &[
    "for example",
    "for instance",
    "such as",
    "let's say",
    "say you",
    "imagine",
    "like when",
    "one time",
    "last week",
    "last year",
    "here's how",
    "case in point",
    "story",
    "i remember",
    "picture this",
    "as an example",
];
const CTA: &[&str] = &[
    "subscribe",
    "follow",
    "like and",
    "comment below",
    "link in",
    "check out",
    "sign up",
    "download",
    "join",
    "share this",
    "let me know",
    "see you",
    "thanks for watching",
    "click",
    "hit the",
    "turn on",
    "visit",
];
const POINT: &[&str] = &[
    "the key",
    "the reason",
    "because",
    "that means",
    "takeaway",
    "important",
    "remember",
    "the point",
    "bottom line",
    "in short",
    "the answer",
    "the secret",
    "what matters",
    "so basically",
    "here's why",
    "the truth",
    "you need to",
    "you should",
    "make sure",
    "never",
    "always",
];
const HOOK: &[&str] = &[
    "stop", "never", "secret", "mistake", "truth", "nobody", "everyone", "biggest", "worst",
    "best", "how to", "why", "what if", "imagine", "watch",
];

fn sentences(words: &[Wd], cfg: &BeatConfig) -> Vec<Sentence> {
    let mut out: Vec<Sentence> = Vec::new();
    let mut first = 0;
    for i in 0..words.len() {
        let last = i + 1 == words.len();
        let gap = if last {
            i64::MAX
        } else {
            words[i + 1].start - words[i].end
        };
        let count = i + 1 - first;
        if ends_sentence(&words[i].text)
            || gap >= cfg.sentence_pause_ms
            || count >= cfg.max_sentence_words
            || last
        {
            let slice = &words[first..=i];
            let norms: Vec<&str> = slice
                .iter()
                .map(|w| w.norm.as_str())
                .filter(|n| !n.is_empty())
                .collect();
            out.push(Sentence {
                first,
                end: i + 1,
                start_ms: words[first].start,
                end_ms: words[i].end,
                padded: format!(" {} ", norms.join(" ")),
                words: count,
                question: words[i].text.trim_end().ends_with('?'),
            });
            first = i + 1;
        }
    }
    out
}

fn cues<'a>(padded: &str, list: &[&'a str]) -> Vec<&'a str> {
    list.iter()
        .copied()
        .filter(|c| padded.contains(&format!(" {} ", phrase_words(c).join(" "))))
        .collect()
}

struct Scored {
    label: BeatLabel,
    confidence: f32,
    scores: [f64; 5],
    cues: Vec<String>,
    hook_score: f32,
}

fn score(s: &Sentence, idx: usize, total: usize, duration: i64, cfg: &BeatConfig) -> Scored {
    let f = s.start_ms as f64 / duration.max(1) as f64;
    let mut found: Vec<String> = Vec::new();
    let mut take = |list: &[&str]| -> usize {
        let c = cues(&s.padded, list);
        found.extend(c.iter().map(|x| x.to_string()));
        c.len()
    };
    let greeting = take(GREETING);
    let setup_c = take(SETUP);
    let example_c = take(EXAMPLE);
    let cta_c = take(CTA);
    let point_c = take(POINT);
    let hook_c = take(HOOK);
    let digit = s.padded.chars().any(|c| c.is_ascii_digit());
    let you = s.padded.contains(" you ") || s.padded.contains(" your ");

    let mut hook = 0.0;
    if idx == 0 {
        hook += 0.7;
    } else if s.start_ms <= cfg.hook_window_ms {
        hook += 0.35;
    }
    if s.question {
        hook += 0.8;
    }
    if digit {
        hook += 0.4;
    }
    if you {
        hook += 0.4;
    }
    hook += 0.5 * hook_c.min(2) as f64;
    if s.words <= 22 {
        hook += 0.3;
    }
    if greeting > 0 {
        hook -= 0.8;
    }
    if s.start_ms > cfg.hook_window_ms {
        hook -= 1.0;
    }
    let mut setup = 0.6 * setup_c.min(2) as f64 + 0.9 * greeting.min(1) as f64;
    if f < 0.35 {
        setup += 0.3;
    }
    let example = 0.8 * example_c.min(2) as f64;
    let mut cta = 0.8 * cta_c.min(2) as f64;
    if cta_c > 0 {
        if f > 0.8 {
            cta += 0.5;
        }
        if idx + 1 == total {
            cta += 0.4;
        }
    } else {
        cta = 0.0; // a CTA needs a CTA cue
    }
    let point = 0.3 + 0.5 * point_c.min(2) as f64;
    let scores = [hook, setup, point, example, cta];
    let labels = [
        BeatLabel::Hook,
        BeatLabel::Setup,
        BeatLabel::Point,
        BeatLabel::Example,
        BeatLabel::Cta,
    ];
    let mut order: Vec<usize> = (0..5).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
    let (top, second) = (scores[order[0]], scores[order[1]]);
    let confidence = if top <= 0.0 {
        0.2
    } else {
        (0.4 + 0.6 * (top - second.max(0.0)) / top).clamp(0.0, 1.0) as f32
    };
    // Hook quality (old hook ranking: specificity, promise, self-containment).
    let specificity = if digit || hook_c > 0 { 1.0 } else { 0.4 };
    let promise = if s.question || hook_c > 0 || you {
        0.9
    } else {
        0.4
    };
    let self_contained = if s.words >= 4 && s.words <= 25 {
        1.0
    } else {
        0.5
    };
    Scored {
        label: labels[order[0]],
        confidence,
        scores,
        cues: found,
        hook_score: ((specificity + promise + self_contained) / 3.0) as f32,
    }
}

pub(crate) fn segment(words: &[Wd], duration_ms: i64, cfg: &BeatConfig) -> Vec<Beat> {
    if !cfg.enabled || words.is_empty() {
        return Vec::new();
    }
    let sents = sentences(words, cfg);
    let total = sents.len();
    let scored: Vec<Scored> = sents
        .iter()
        .enumerate()
        .map(|(i, s)| score(s, i, total, duration_ms, cfg))
        .collect();
    let mut beats: Vec<Beat> = Vec::new();
    let mut group: Vec<usize> = Vec::new();
    let flush = |group: &mut Vec<usize>, beats: &mut Vec<Beat>| {
        let (Some(&a), Some(&b)) = (group.first(), group.last()) else {
            return;
        };
        let label = scored[a].label;
        let mut scores = BTreeMap::new();
        for (name, k) in [
            ("hook", 0),
            ("setup", 1),
            ("point", 2),
            ("example", 3),
            ("cta", 4),
        ] {
            let mean = group.iter().map(|&g| scored[g].scores[k]).sum::<f64>() / group.len() as f64;
            scores.insert(name.to_string(), round2(mean));
        }
        let mut cue_list: Vec<String> =
            group.iter().flat_map(|&g| scored[g].cues.clone()).collect();
        cue_list.sort();
        cue_list.dedup();
        let confidence =
            group.iter().map(|&g| scored[g].confidence).sum::<f32>() / group.len() as f32;
        let (first, end) = (sents[a].first, sents[b].end);
        beats.push(Beat {
            id: format!("beat-{:03}", beats.len() + 1),
            label,
            start_ms: sents[a].start_ms,
            end_ms: sents[b].end_ms,
            word_range: [first, end],
            sentences: group.len(),
            text: crate::editorial::util::join(words, first, end),
            confidence,
            scores,
            cues: cue_list,
            hook_score: (label == BeatLabel::Hook).then(|| {
                group.iter().map(|&g| scored[g].hook_score).sum::<f32>() / group.len() as f32
            }),
        });
        group.clear();
    };
    for (i, s) in scored.iter().enumerate() {
        let join_prev = group.last().is_some_and(|&p| {
            scored[p].label == s.label
                && sents[i].start_ms - sents[p].end_ms < cfg.beat_gap_ms
                // A hook is at most two sentences.
                && !(s.label == BeatLabel::Hook && group.len() >= 2)
        });
        if !join_prev {
            flush(&mut group, &mut beats);
        }
        group.push(i);
    }
    flush(&mut group, &mut beats);
    beats
}
