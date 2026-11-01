//! rightkit-align: align a written script to timed ASR words and check what the crate promises: accepted tokens
//! carry the ASR span (ms and samples), an omitted word is `Unaligned`, a misheard name is `WordDiffers` and fails
//! designated coverage, a repeat with equal candidates is `Ambiguous`, numbers/contractions match their spoken forms,
//! boundary error stays inside tolerance on the 80 ms lattice, and the `rightkit.alignment` v1 document round trips
//! and refuses another version. The ASR words are data (no engine runs); no asset needed.
use rightkit_align::{
    align, asr_words_from_seconds, boundary_errors, AlignOptions, AlignmentContract, Boundary,
    DesignatedAnchor, Outcome, RejectReason, ScriptSpec, CONTRACT, CONTRACT_VERSION, LATTICE_MS,
    MIN_WORD_MS, TIMING_TOLERANCE_MS,
};
use serde_json::{json, Value};

const RATE: u32 = 16_000;

fn lattice(ms: u64) -> u64 {
    (ms + LATTICE_MS / 2) / LATTICE_MS * LATTICE_MS
}

/// ASR words as Parakeet reports them: boundaries on the 80 ms lattice, at least 80 ms long.
fn heard(words: &[(&str, u64, u64)]) -> Vec<rightkit_transcript::Word> {
    asr_words_from_seconds(words.iter().map(|&(t, s, e)| {
        let start = lattice(s);
        let end = lattice(e).max(start + MIN_WORD_MS);
        (t, start as f32 / 1000.0, end as f32 / 1000.0)
    }))
}

fn designate(word: &str, target: &str) -> DesignatedAnchor {
    DesignatedAnchor {
        word_id: word.into(),
        target_id: target.into(),
    }
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let opts = AlignOptions::default();

    // Clean narration with a control name, a decimal, and a repeated "the" in distinct contexts.
    let truth: &[(&str, u64, u64)] = &[
        ("click", 400, 700),
        ("export", 760, 1180),
        ("then", 1300, 1560),
        ("open", 1640, 1900),
        ("the", 1940, 2020),
        ("postgresql", 2060, 2620),
        ("settings", 2660, 3100),
        ("and", 3140, 3260),
        ("set", 3300, 3480),
        ("the", 3520, 3600),
        ("timeout", 3640, 4080),
        ("to", 4120, 4220),
        ("2.5", 4260, 4700),
        ("seconds", 4740, 5200),
    ];
    let script = ScriptSpec::from_text(
        "step-1",
        "Click Export, then open the PostgreSQL settings and set the timeout to 2.5 seconds.",
    );
    let clean = align(
        &script,
        &heard(truth),
        RATE,
        &[designate("w0", "t-click"), designate("w1", "t-export")],
        &opts,
    );
    if clean.anchors.len() != 14 {
        return Err(format!("expected 14 anchors, got {}", clean.anchors.len()));
    }
    for a in &clean.anchors {
        if !matches!(a.outcome, Outcome::Accepted { .. }) || a.lexical_confidence != 1.0 {
            return Err(format!(
                "token {} {:?} was not cleanly accepted: {:?}",
                a.word_id, a.text, a.outcome
            ));
        }
        if (a.timing_confidence - 0.625).abs() > 1e-6 {
            return Err(format!(
                "timing confidence of {} is {}, the 50/80 lattice bound is 0.625",
                a.word_id, a.timing_confidence
            ));
        }
    }
    if !clean.coverage.meets_floor() || clean.coverage.accepted != 2 || clean.clauses.len() != 2 {
        return Err(format!(
            "coverage {:?}, clauses {}",
            clean.coverage,
            clean.clauses.len()
        ));
    }
    let end = clean.clauses[1].end;
    if (end.end_ms, end.end_sample, end.sample_rate) != (5200, 5200 * 16, RATE) {
        return Err(format!("last clause end {end:?}"));
    }
    let truths: Vec<Boundary> = truth
        .iter()
        .enumerate()
        .map(|(i, &(_, s, e))| Boundary {
            word_id: format!("w{i}"),
            start_ms: s,
            end_ms: e,
        })
        .collect();
    let errors = boundary_errors(&clean, &truths);
    if errors.len() != truths.len() || errors.iter().any(|e| !e.within_tolerance) {
        return Err(format!(
            "boundary errors outside the {TIMING_TOLERANCE_MS} ms tolerance: {errors:?}"
        ));
    }

    // An omitted word is Unaligned; neighbours still align.
    let omitted = align(
        &ScriptSpec::from_text("s", "Click please Save now"),
        &heard(&[("click", 0, 300), ("save", 400, 700), ("now", 800, 1000)]),
        RATE,
        &[designate("w2", "t-save")],
        &opts,
    );
    if omitted.anchor("w1").map(|a| a.outcome)
        != Some(Outcome::Rejected {
            reason: RejectReason::Unaligned,
        })
        || omitted.accepted_span("w0").is_none()
        || omitted.accepted_span("w2").is_none()
        || !omitted.coverage.meets_floor()
    {
        return Err(format!(
            "omitted word handling: {:?}",
            omitted
                .anchors
                .iter()
                .map(|a| (&a.text, a.outcome))
                .collect::<Vec<_>>()
        ));
    }

    // A misheard designated name is rejected and fails coverage, though its neighbours pass.
    let misheard = align(
        &ScriptSpec::from_text("s", "Open PostgreSQL now"),
        &heard(&[
            ("open", 0, 300),
            ("postgres", 400, 1000),
            ("now", 1100, 1300),
        ]),
        RATE,
        &[designate("w0", "t-open"), designate("w1", "t-pg")],
        &opts,
    );
    let product = misheard.anchor("w1").ok_or("no anchor w1")?;
    if product.outcome
        != (Outcome::Rejected {
            reason: RejectReason::WordDiffers,
        })
        || product.asr_text.as_deref() != Some("postgres")
        || misheard.coverage.meets_floor()
        || misheard.coverage.accepted != 1
        || misheard.coverage.failures.len() != 1
        || misheard.coverage.failures[0].target_id != "t-pg"
    {
        return Err(format!(
            "misheard name: {:?} coverage {:?}",
            product, misheard.coverage
        ));
    }

    // Two equally good candidates: ambiguous, not guessed.
    let repeat = align(
        &ScriptSpec::from_text("s", "go the"),
        &heard(&[("go", 0, 300), ("the", 400, 600), ("the", 700, 900)]),
        RATE,
        &[],
        &opts,
    );
    if repeat.anchor("w1").map(|a| a.outcome)
        != Some(Outcome::Rejected {
            reason: RejectReason::Ambiguous,
        })
    {
        return Err(
            "a repeated word with two equal candidates was not rejected as Ambiguous".into(),
        );
    }

    // Contractions and digits match their spoken forms.
    let spoken = align(
        &ScriptSpec::from_text("s", "Don't set 3 items"),
        &heard(&[
            ("do", 0, 200),
            ("not", 240, 400),
            ("set", 440, 700),
            ("three", 740, 1000),
            ("items", 1040, 1400),
        ]),
        RATE,
        &[],
        &opts,
    );
    if spoken
        .anchors
        .iter()
        .any(|a| !matches!(a.outcome, Outcome::Accepted { .. }))
    {
        return Err(format!(
            "contraction/number alignment: {:?}",
            spoken
                .anchors
                .iter()
                .map(|a| (&a.text, a.outcome))
                .collect::<Vec<_>>()
        ));
    }

    // The versioned document round trips and refuses another version.
    let doc = serde_json::to_value(clean.to_contract())
        .map_err(|e| format!("contract serialise: {e}"))?;
    if doc["contract"] != CONTRACT || doc["contractVersion"] != CONTRACT_VERSION {
        return Err(format!(
            "contract tags {} {}",
            doc["contract"], doc["contractVersion"]
        ));
    }
    let back: AlignmentContract =
        serde_json::from_value(doc.clone()).map_err(|e| format!("contract read-back: {e}"))?;
    if serde_json::to_value(&back).map_err(|e| format!("re-serialise: {e}"))? != doc {
        return Err("the contract document did not round-trip".into());
    }
    let mut future = doc.clone();
    future["contractVersion"] = json!("2.0.0");
    if serde_json::from_value::<AlignmentContract>(future).is_ok() {
        return Err("a 2.0.0 document was accepted".into());
    }

    Ok(json!({
        "tokens": clean.anchors.len(),
        "clauses": clean.clauses.len(),
        "timing_confidence": clean.anchors[0].timing_confidence,
        "misheard": "word_differs",
        "omitted": "unaligned",
        "repeat": "ambiguous",
        "contract_version": CONTRACT_VERSION,
    }))
}
