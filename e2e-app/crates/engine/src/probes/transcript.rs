//! rightkit-transcript: segment timed words into caption cues, export SRT and VTT, parse them back, and round-trip
//! the Transcript JSON contract. No asset needed: the crate is pure.
use rightkit_transcript::{
    build_cues, check_alignment, parse_srt, parse_timing_json, parse_vtt, to_plain_text, to_srt, to_timing_json,
    to_vtt, words_from_cues, CaptionConfig, EndSource, ModelRef, Newline, Profile, TimedWord, Transcript,
    ALIGNMENT_TOLERANCE_MS,
};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    // 48 words, 300 ms each, a 1.5 s pause after word 23, a sentence end every 12 words.
    let mut words = Vec::new();
    let mut t = 0u64;
    for i in 0..48 {
        if i == 24 {
            t += 1500;
        }
        let punct = if i % 12 == 11 { "." } else { "" };
        words.push(TimedWord::new(format!("word{i}{punct}"), t, t + 300));
        t += 300;
    }

    let cfg = CaptionConfig::for_profile(Profile::Standard);
    let cues = build_cues(&words, &cfg);
    if cues.len() < 3 {
        return Err(format!("expected at least 3 cues from 48 words and a long pause, got {}", cues.len()));
    }
    let mut covered = 0usize;
    for (i, c) in cues.iter().enumerate() {
        if c.lines.len() > cfg.max_lines {
            return Err(format!("cue {i} has {} lines, limit {}", c.lines.len(), cfg.max_lines));
        }
        if c.end_ms <= c.start_ms || c.end_ms - c.start_ms > cfg.max_cue_ms {
            return Err(format!("cue {i} duration {}..{} breaks limits", c.start_ms, c.end_ms));
        }
        if c.lines.iter().any(|l| l.chars().count() > cfg.max_chars_per_line) {
            return Err(format!("cue {i} has an over-long line"));
        }
        if i > 0 && c.start_ms < cues[i - 1].end_ms {
            return Err(format!("cue {i} overlaps the previous cue"));
        }
        covered += c.last_word - c.first_word + 1;
    }
    if covered != words.len() {
        return Err(format!("cues cover {covered} of {} words", words.len()));
    }
    let issues = check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS);
    if !issues.is_empty() {
        return Err(format!("cue edges are off word boundaries: {issues:?}"));
    }
    // The 1.5 s pause must break a cue between word 23 and word 24.
    if !cues.iter().any(|c| c.last_word == 23) {
        return Err("the long pause did not end a cue at word 23".into());
    }

    // SRT round trip (LF and CRLF) and VTT round trip.
    let dir = ctx.dir("transcript")?;
    let mut formats = Vec::new();
    for (name, nl) in [("lf", Newline::Lf), ("crlf", Newline::Crlf)] {
        let srt = to_srt(&cues, nl);
        let path = dir.join(format!("{name}.srt"));
        std::fs::write(&path, &srt).map_err(|e| format!("write {}: {e}", path.display()))?;
        let back = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let parsed = parse_srt(&back).map_err(|e| format!("parse_srt({name}): {e}"))?;
        compare(&cues, &parsed, &format!("srt {name}"))?;
        formats.push(format!("srt-{name}"));
    }
    let vtt = to_vtt(&cues, Newline::Lf);
    if !vtt.starts_with("WEBVTT") {
        return Err("to_vtt output lacks the WEBVTT header".into());
    }
    let vpath = dir.join("out.vtt");
    std::fs::write(&vpath, &vtt).map_err(|e| format!("write {}: {e}", vpath.display()))?;
    let vback = std::fs::read_to_string(&vpath).map_err(|e| format!("read {}: {e}", vpath.display()))?;
    let vparsed = parse_vtt(&vback).map_err(|e| format!("parse_vtt: {e}"))?;
    compare(&cues, &vparsed, "vtt")?;

    // Plain text carries every word.
    let plain = to_plain_text(&cues);
    for w in &words {
        if !plain.contains(&w.text) {
            return Err(format!("plain text lacks {:?}", w.text));
        }
    }

    // Words recovered from imported cues tile each cue and never overlap.
    let recovered = words_from_cues(&vparsed);
    if recovered.len() != words.len() {
        return Err(format!("words_from_cues gave {} words, expected {}", recovered.len(), words.len()));
    }
    for pair in recovered.windows(2) {
        if pair[1].start_ms < pair[0].end_ms {
            return Err("recovered words overlap".into());
        }
    }

    // Timing JSON round trip (seconds in the file, ms back).
    let timing = to_timing_json(&cues, &words);
    let tw = parse_timing_json(&timing).map_err(|e| format!("parse_timing_json: {e}"))?;
    if tw.len() != words.len() || tw.iter().zip(&words).any(|(a, b)| a.text != b.text || a.start_ms != b.start_ms || a.end_ms != b.end_ms) {
        return Err("timing JSON did not round-trip the words".into());
    }

    // Transcript contract: build, validate, serialise, strict read-back, then reject an overlap.
    let model = ModelRef { id: "e2e-probe".into(), revision: "1".into(), backend: "synthetic".into() };
    let tr = Transcript::from_words(&words, "en", None, model, EndSource::Model);
    tr.validate().map_err(|e| format!("validate: {e}"))?;
    let json_text = tr.to_json();
    let back = Transcript::from_json(&json_text).map_err(|e| format!("from_json: {e}"))?;
    if back != tr {
        return Err("Transcript JSON did not round-trip".into());
    }
    if back.timed_words() != words {
        return Err("timed_words() differs from the input words".into());
    }
    let mut bad = tr.clone();
    bad.words[1].start_ms = bad.words[0].start_ms;
    if bad.validate().is_ok() {
        return Err("validate accepted overlapping words".into());
    }

    Ok(json!({
        "words": words.len(),
        "cues": cues.len(),
        "roundtrips": formats,
        "vtt_bytes": vtt.len(),
        "transcript_json_bytes": json_text.len(),
    }))
}

fn compare(cues: &[rightkit_transcript::Cue], parsed: &[rightkit_transcript::ParsedCue], what: &str) -> Result<(), String> {
    if parsed.len() != cues.len() {
        return Err(format!("{what}: {} cues parsed, {} written", parsed.len(), cues.len()));
    }
    for (i, (c, p)) in cues.iter().zip(parsed).enumerate() {
        if (c.start_ms, c.end_ms) != (p.start_ms, p.end_ms) || c.lines != p.lines {
            return Err(format!("{what}: cue {i} changed: wrote {:?} {}..{}, read {:?} {}..{}", c.lines, c.start_ms, c.end_ms, p.lines, p.start_ms, p.end_ms));
        }
    }
    Ok(())
}
