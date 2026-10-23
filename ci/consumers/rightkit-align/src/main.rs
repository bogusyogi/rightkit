// rightkit-align 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the local script-to-ASR alignment family (rightkit-align lib.rs): ScriptSpec::from_text
// and validate; asr_words_from_seconds; align with default AlignOptions; Alignment::anchor,
// accepted_span and to_contract; Coverage::meets_floor; an accepted designated token with its
// span in milliseconds and samples; a designated token the ASR omitted, refused as Unaligned and
// kept out of coverage; the contract identity keys.
// Does not prove: contraction or number normalisation beyond the tokens named here; ambiguity
// rejection; WordDiffers; clause output; boundary_errors against truth timings; lattice
// confidence values; the JSON schema or serde round trips; the CutRight fixtures on narration audio.
// State: pure computation over caller-provided words. No audio, no models, no files, no network,
// no real user data.
use rightkit_align::{
    align, asr_words_from_seconds, AlignOptions, DesignatedAnchor, Outcome, RejectReason,
    ScriptSpec, CONTRACT, CONTRACT_VERSION,
};

const SAMPLE_RATE: u32 = 16_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Script "Open the file": w0 "Open", w1 "the", w2 "file", all in step-1.
    let script = ScriptSpec::from_text("step-1", "Open the file");
    script.validate()?;
    assert_eq!(script.tokens.len(), 3, "three script tokens");
    assert_eq!(script.tokens[2].word_id, "w2");

    // The ASR returned "open" and "file" only. "the" was omitted.
    let asr = asr_words_from_seconds([("open", 0.00, 0.40), ("file", 0.56, 0.96)]);
    assert_eq!(asr.len(), 2, "two ASR words");

    let options = AlignOptions::default();
    let designated = [
        DesignatedAnchor {
            word_id: "w2".into(),
            target_id: "file-field".into(),
        },
        DesignatedAnchor {
            word_id: "w1".into(),
            target_id: "the-field".into(),
        },
    ];
    let alignment = align(&script, &asr, SAMPLE_RATE, &designated, &options);

    // One anchor per script token, in script order, echoing its identities.
    assert_eq!(alignment.anchors.len(), 3, "one anchor per script token");
    let file = alignment.anchor("w2").ok_or("w2 anchor")?;
    assert_eq!(file.text, "file");
    assert_eq!(file.target_id.as_deref(), Some("file-field"));

    // The accepted span: milliseconds and samples at 16 kHz (16 samples per millisecond).
    let span = alignment.accepted_span("w2").ok_or("w2 accepted")?;
    assert_eq!(span.sample_rate, SAMPLE_RATE);
    assert_eq!(span.start_sample, span.start_ms * 16);
    assert_eq!(span.end_sample, span.end_ms * 16);

    // The omitted token is rejected as unaligned and never satisfies coverage.
    let the = alignment.anchor("w1").ok_or("w1 anchor")?;
    assert!(
        matches!(
            the.outcome,
            Outcome::Rejected {
                reason: RejectReason::Unaligned
            }
        ),
        "the omitted word is rejected as unaligned"
    );
    assert!(alignment.accepted_span("w1").is_none());

    assert_eq!(alignment.coverage.designated, 2, "two designations");
    assert_eq!(alignment.coverage.accepted, 1, "one designation accepted");
    assert!(
        !alignment.coverage.meets_floor(),
        "a rejected designated anchor fails the coverage floor"
    );
    assert_eq!(alignment.coverage.failures.len(), 1);
    assert_eq!(alignment.coverage.failures[0].word_id, "w1");
    assert_eq!(alignment.coverage.failures[0].target_id, "the-field");

    // The versioned document carries the same tokens and the tolerance the alignment used.
    let document = alignment.to_contract();
    assert_eq!(document.tokens.len(), 3);
    assert_eq!(document.timing_tolerance_ms, options.tolerance_ms);
    assert_eq!(CONTRACT, "rightkit.alignment");
    assert_eq!(CONTRACT_VERSION, "1.0.0");

    println!("OK rightkit-align 0.1.0 (registry consumer)");
    Ok(())
}
