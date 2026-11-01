//! Disfluency detection beyond fillers: repetitions, stutters, self-corrections
//! and false starts. Heuristic and deterministic; hesitation sounds are
//! ignored when matching, and ranges are always whole transcript words.

use crate::editorial::config::DisfluencyConfig;
use crate::editorial::item::{word_draft, Draft, ItemKind, Tier};
use crate::editorial::util::{
    dice, ends_interrupted, ends_sentence, is_hesitation, phrase_at, phrase_words, round2, Wd,
};

pub(crate) fn detect(words: &[Wd], cfg: &DisfluencyConfig) -> Vec<Draft> {
    let toks: Vec<usize> = (0..words.len())
        .filter(|&i| !words[i].norm.is_empty() && !is_hesitation(&words[i].norm))
        .collect();
    let mut out = Vec::new();
    if cfg.repetitions {
        repetitions(words, &toks, cfg, &mut out);
    }
    if cfg.stutters {
        stutters(words, &toks, cfg, &mut out);
    }
    if cfg.self_corrections || cfg.false_starts {
        corrections(words, &toks, cfg, &mut out);
    }
    out
}

fn norm<'a>(words: &'a [Wd], toks: &[usize], p: usize) -> &'a str {
    &words[toks[p]].norm
}

fn same_seq(words: &[Wd], toks: &[usize], a: usize, b: usize, n: usize) -> bool {
    (0..n).all(|k| norm(words, toks, a + k) == norm(words, toks, b + k))
}

fn gap(words: &[Wd], toks: &[usize], last_of_first: usize, first_of_next: usize) -> i64 {
    words[toks[first_of_next]].start - words[toks[last_of_first]].end
}

fn repetitions(words: &[Wd], toks: &[usize], cfg: &DisfluencyConfig, out: &mut Vec<Draft>) {
    let m = toks.len();
    let mut i = 0;
    while i < m {
        let mut hit = false;
        for n in (1..=cfg.max_ngram.max(1)).rev() {
            if i + 2 * n > m || !same_seq(words, toks, i, i + n, n) {
                continue;
            }
            let g = gap(words, toks, i + n - 1, i + n);
            if g > cfg.repetition_max_gap_ms || ends_sentence(&words[toks[i + n - 1]].text) {
                continue;
            }
            if n == 1 {
                let t = norm(words, toks, i);
                if cfg.intentional_repeats.iter().any(|r| r == t)
                    || t.chars().all(|c| c.is_ascii_digit())
                {
                    continue;
                }
            }
            // Extend over further copies.
            let mut copies = 2;
            while i + (copies + 1) * n <= m
                && same_seq(words, toks, i + (copies - 1) * n, i + copies * n, n)
                && gap(words, toks, i + copies * n - 1, i + copies * n) <= cfg.repetition_max_gap_ms
                && !ends_sentence(&words[toks[i + copies * n - 1]].text)
            {
                copies += 1;
            }
            let last_copy = i + (copies - 1) * n;
            let (first, end) = (toks[i], toks[last_copy]);
            let mut conf = if n >= 2 { 0.85 } else { 0.8 };
            if g > 300 {
                conf -= 0.1;
            }
            let mut d = word_draft(
                words,
                ItemKind::Repetition,
                first,
                end,
                Tier::Auto,
                conf,
                format!(
                    "\"{}\" repeated {copies}x; last copy kept",
                    crate::editorial::util::join(
                        words,
                        toks[last_copy],
                        toks[last_copy + n - 1] + 1
                    )
                ),
            );
            d.evidence.kept_text = Some(crate::editorial::util::join(
                words,
                toks[last_copy],
                toks[last_copy + n - 1] + 1,
            ));
            d.evidence.signals.insert("copies".into(), copies as f64);
            d.evidence.signals.insert("ngram".into(), n as f64);
            d.evidence.signals.insert("gap_ms".into(), g as f64);
            out.push(d);
            i = last_copy;
            hit = true;
            break;
        }
        if !hit {
            i += 1;
        }
    }
}

fn is_fragment_of(frag: &str, whole: &str) -> bool {
    !frag.is_empty() && whole.len() > frag.len() && whole.starts_with(frag)
}

fn stutters(words: &[Wd], toks: &[usize], cfg: &DisfluencyConfig, out: &mut Vec<Draft>) {
    let m = toks.len();
    let mut t = 0;
    while t + 1 < m {
        // Dash-marked fragment chain: "th- the", "b- b- but".
        let dashed = |p: usize| {
            let w = &words[toks[p]];
            ends_interrupted(&w.text)
                && !w.text.ends_with(['\u{2026}'])
                && !w.norm.is_empty()
                && w.norm.chars().count() <= cfg.stutter_max_fragment
        };
        if dashed(t) {
            let mut u = t;
            while u + 1 < m && dashed(u) && {
                let nx = norm(words, toks, u + 1);
                is_fragment_of(norm(words, toks, u), nx) || norm(words, toks, u) == nx
            } {
                u += 1;
            }
            let target = norm(words, toks, u);
            if u > t || is_fragment_of(norm(words, toks, t), norm(words, toks, t + 1)) {
                let end_p = if u > t { u } else { t + 1 };
                let first = toks[t];
                let end = toks[end_p];
                let mut d = word_draft(
                    words,
                    ItemKind::Stutter,
                    first,
                    end,
                    Tier::Auto,
                    0.85,
                    format!(
                        "word fragment before \"{}\"",
                        words[toks[end_p]]
                            .text
                            .trim_matches(|c: char| !c.is_alphanumeric())
                    ),
                );
                d.evidence.kept_text = Some(words[toks[end_p]].text.clone());
                let _ = target;
                out.push(d);
                t = end_p;
                continue;
            }
        }
        // Undashed: "b b but" (short repeated fragment then a word it starts).
        let x = norm(words, toks, t);
        if x.chars().count() <= 2 && !matches!(x, "a" | "i") && x.chars().all(|c| c.is_alphabetic())
        {
            let mut u = t;
            while u + 1 < m && norm(words, toks, u + 1) == x {
                u += 1;
            }
            if u > t && u + 1 < m && is_fragment_of(x, norm(words, toks, u + 1)) {
                let first = toks[t];
                let end = toks[u + 1];
                let mut d = word_draft(
                    words,
                    ItemKind::Stutter,
                    first,
                    end,
                    Tier::Auto,
                    0.7,
                    format!("stuttered \"{x}\" before \"{}\"", norm(words, toks, u + 1)),
                );
                d.evidence.kept_text = Some(words[toks[u + 1]].text.clone());
                out.push(d);
                t = u + 1;
                continue;
            }
        }
        t += 1;
    }
}

fn marker_len(words: &[Wd], toks: &[usize], pos: usize, markers: &[Vec<String>]) -> Option<usize> {
    markers
        .iter()
        .filter(|m| phrase_at(words, toks, pos, m))
        .map(|m| m.len())
        .max()
}

fn corrections(words: &[Wd], toks: &[usize], cfg: &DisfluencyConfig, out: &mut Vec<Draft>) {
    let m = toks.len();
    let mut markers: Vec<Vec<String>> = cfg
        .correction_markers
        .iter()
        .map(|s| phrase_words(s))
        .filter(|p| !p.is_empty())
        .collect();
    markers.sort_by_key(|p| std::cmp::Reverse(p.len()));
    for t in 0..m.saturating_sub(1) {
        let w = &words[toks[t]];
        let dash = ends_interrupted(&w.text);
        let mlen = marker_len(words, toks, t + 1, &markers);
        let r = t + 1 + mlen.unwrap_or(0);
        if r >= m {
            continue;
        }
        let pause = !ends_sentence(&w.text)
            && words[toks[t + 1]].start - w.end >= cfg.interrupt_pause_ms
            && mlen.is_none();
        if !(dash || mlen.is_some() || pause) {
            continue;
        }
        if mlen.is_none() && ends_sentence(&w.text) {
            continue;
        }
        // A bare fragment ("th- the") is a stutter, not a correction.
        if dash
            && mlen.is_none()
            && w.norm.chars().count() <= cfg.stutter_max_fragment
            && is_fragment_of(&w.norm, norm(words, toks, t + 1))
        {
            continue;
        }
        let mut cs = t;
        while cs > 0
            && t - cs + 1 < cfg.correction_max_words
            && !ends_sentence(&words[toks[cs - 1]].text)
        {
            cs -= 1;
        }
        let explicit = dash || mlen.is_some();
        let restart_norm = norm(words, toks, r);
        let mut found = None;
        for p in (cs..=t).rev() {
            if norm(words, toks, p) != restart_norm {
                continue;
            }
            let l = t - p + 1;
            let lr = l.min(m - r);
            let a: Vec<&str> = (p..=t).map(|k| norm(words, toks, k)).collect();
            let b: Vec<&str> = (r..r + lr).map(|k| norm(words, toks, k)).collect();
            let sim = dice(&a, &b);
            if sim >= cfg.correction_min_similarity {
                found = Some((p, l, sim, a == b));
                break;
            }
        }
        match found {
            Some((_, _, _, exact)) if exact && mlen.is_none() => {
                // Plain repetition: the repetition detector owns it.
            }
            Some((p, l, sim, _)) if cfg.self_corrections => {
                let (first, end) = (toks[p], toks[r]);
                let lr = l.min(m - r);
                let tier = if explicit && !(l >= 6 && sim < 0.7) {
                    Tier::Auto
                } else {
                    Tier::Suggest
                };
                let conf = 0.55 + 0.4 * sim - if explicit { 0.0 } else { 0.15 };
                let kept = crate::editorial::util::join(words, toks[r], toks[r + lr - 1] + 1);
                let mut d = word_draft(
                    words,
                    ItemKind::SelfCorrection,
                    first,
                    end,
                    tier,
                    conf,
                    format!("abandoned clause restarted as \"{kept}\""),
                );
                d.evidence.kept_text = Some(kept);
                d.evidence
                    .signals
                    .insert("similarity".into(), round2(sim as f64));
                d.evidence
                    .signals
                    .insert("abandoned_words".into(), l as f64);
                if let Some(ml) = mlen {
                    d.evidence.notes.push(format!(
                        "spoken cue \"{}\"",
                        crate::editorial::util::join(words, toks[t + 1], toks[t + ml] + 1)
                    ));
                } else if !dash {
                    d.evidence.notes.push("pause-only break".into());
                }
                out.push(d);
            }
            Some(_) => {}
            None => {
                if cfg.self_corrections && mlen.is_some() && !ends_sentence(&w.text) && !dash {
                    // "go left, I mean right": drop the replaced word and the cue.
                    let (first, end) = (toks[t], toks[r]);
                    let kept = words[toks[r]].text.clone();
                    let mut d = word_draft(
                        words,
                        ItemKind::SelfCorrection,
                        first,
                        end,
                        Tier::Suggest,
                        0.5,
                        format!("\"{}\" replaced by \"{kept}\"", w.text),
                    );
                    d.evidence.kept_text = Some(kept);
                    d.evidence.notes.push("substitution".into());
                    out.push(d);
                } else if cfg.false_starts && dash && mlen.is_none() {
                    let mut e = r;
                    while e + 1 < m && !ends_sentence(&words[toks[e]].text) {
                        e += 1;
                    }
                    let replacement = e + 1 - r;
                    let l = t - cs + 1;
                    if replacement >= cfg.false_start_min_replacement {
                        let tier = if l <= cfg.false_start_auto_words {
                            Tier::Auto
                        } else {
                            Tier::Suggest
                        };
                        let kept = crate::editorial::util::join(words, toks[r], toks[e] + 1);
                        let mut d = word_draft(
                            words,
                            ItemKind::FalseStart,
                            toks[cs],
                            toks[r],
                            tier,
                            if tier == Tier::Auto { 0.7 } else { 0.55 },
                            format!("abandoned start replaced by \"{kept}\""),
                        );
                        d.evidence.kept_text = Some(kept);
                        d.evidence
                            .signals
                            .insert("abandoned_words".into(), l as f64);
                        d.evidence
                            .signals
                            .insert("replacement_words".into(), replacement as f64);
                        out.push(d);
                    }
                }
            }
        }
    }
}
