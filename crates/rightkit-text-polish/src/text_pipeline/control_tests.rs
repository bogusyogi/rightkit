// NOTE: `clean_text` excludes the trailing sentence punctuation as well as the
// trigger word -- the tail regex tolerates/consumes punctuation before the
// trigger. That is fine: the invariant under test is that the ROUTING TOKEN can
// never reach the delivered text (including the L2/L3 failure fallback, which
// reuses this exact string).
use super::*;

// Regression for POST_ASR_PROCESSING_ARCHITECTURE.md §1.1 "L2/L3 fallback
// must not reinsert the routing token": the trailing "prompt"/"summarize"
// word is stripped off as ROUTING state by `parse_ai_transform_command`,
// not as transcript cleanup. `clean_text` is the single source both the
// semantic model call AND the deterministic fallback (used when the model
// call fails) are built from in `finalize_transcript.rs`, so asserting the
// trigger is absent from `clean_text` here locks the invariant for both
// the success and the failure/fallback path.

#[test]
fn l2_prompt_trigger_is_stripped_into_routing_state_not_left_in_text() {
    let cmd = parse_ai_transform_command("Compare the three models. prompt")
        .expect("trailing prompt trigger should route to L2");
    assert_eq!(cmd.intent, AiTransformIntent::Prompt);
    assert_eq!(cmd.clean_text, "Compare the three models");
    assert!(
        !cmd.clean_text.to_ascii_lowercase().contains("prompt"),
        "fallback source must not contain the routing token: {:?}",
        cmd.clean_text
    );
}

#[test]
fn l3_summarize_trigger_is_stripped_into_routing_state_not_left_in_text() {
    let cmd = parse_ai_transform_command("Notes from the meeting today. summarize")
        .expect("trailing summarize trigger should route to L3");
    assert_eq!(cmd.intent, AiTransformIntent::Summarize);
    assert_eq!(cmd.clean_text, "Notes from the meeting today");
    assert!(
        !cmd.clean_text.to_ascii_lowercase().contains("summarize"),
        "fallback source must not contain the routing token: {:?}",
        cmd.clean_text
    );
}

#[test]
fn l3_summarise_british_spelling_trigger_is_stripped() {
    let cmd = parse_ai_transform_command("Notes from the meeting today. summarise this")
        .expect("trailing summarise-this trigger should route to L3");
    assert_eq!(cmd.intent, AiTransformIntent::Summarize);
    assert_eq!(cmd.clean_text, "Notes from the meeting today");
    assert!(!cmd.clean_text.to_ascii_lowercase().contains("summaris"));
}

#[test]
fn l3_truncated_summarize_trigger_is_stripped() {
    let cmd = parse_ai_transform_command("Check the last few recordings summariz")
        .expect("observed final-ASR truncation should route to L3");
    assert_eq!(cmd.intent, AiTransformIntent::Summarize);
    assert_eq!(cmd.clean_text, "Check the last few recordings");

    let selected =
        parse_selected_text_ai_transform_command("summariz", Some("Selected text to summarize."))
            .expect("observed bare trigger truncation should use selected text");
    assert_eq!(selected.intent, AiTransformIntent::Summarize);
    assert_eq!(selected.clean_text, "Selected text to summarize.");

    let timed = parse_timed_ai_transform_command(
        "Check the last few recordings summariz",
        &[
            TimedToken {
                text: "Check the last few recordings".into(),
                start: 0.0,
                end: 1.0,
            },
            TimedToken {
                text: " summariz".into(),
                start: 1.4,
                end: 1.8,
            },
        ],
    )
    .expect("timed final-ASR truncation should route to L3");
    assert_eq!(timed.intent, AiTransformIntent::Summarize);
    assert_eq!(timed.clean_text, "Check the last few recordings");
}

#[test]
fn l2_prompt_trigger_preserves_rest_of_dictation_intact() {
    let cmd =
        parse_ai_transform_command("Write a function that reverses a linked list in place. prompt")
            .expect("trailing prompt trigger should route to L2");
    assert_eq!(
        cmd.clean_text,
        "Write a function that reverses a linked list in place"
    );
}

#[test]
fn kws_onset_cut_residue_removes_only_measured_fragments() {
    use super::strip_kws_onset_cut_residue as strip;
    assert_eq!(strip("See if it works, Z"), "See if it works");
    assert_eq!(strip("I switched to cloud, zap"), "I switched to cloud");
    // Live 2026-09-24 (debug build, "...this message zephyr send"): the
    // start of "zephyr" decoded as "sett".
    assert_eq!(
        strip("Fourth test sends this message sett"),
        "Fourth test sends this message"
    );
    assert_eq!(strip("based on your tests is that"), "based on your tests");
    assert_eq!(strip("for now Zephyr Sand ze"), "for now Zephyr Sand");
    // Real endings survive.
    assert_eq!(strip("I don't care, fix it"), "I don't care, fix it");
    assert_eq!(strip("before you do that"), "before you do that");
    assert_eq!(strip("where the gap is"), "where the gap is");
    assert_eq!(strip("That's what it's"), "That's what it's");
    assert_eq!(strip("rename it to x"), "rename it to");
    // The pair only goes when no fragment was dropped first.
    assert_eq!(strip("what is it z"), "what is it");
}
