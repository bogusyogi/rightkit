use super::control::*;
use super::inline::*;
use super::polish::TEST_BRAND;
use super::*;
use std::collections::HashMap;
use std::sync::OnceLock;

/// L0 precision-first: bare ambiguous nouns must NOT become punctuation.
/// Regression for the silent-corruption class fixed in `inline_replacements`
/// ("five star review" -> "five * review", "colon cancer" -> ": cancer", ...).
#[test]
fn bare_ambiguous_symbol_words_stay_lexical() {
    assert_eq!(
        deterministic_polish("we waited a period of time"),
        "We waited a period of time"
    );
    assert_eq!(
        deterministic_polish("the dot product is wrong"),
        "The dot product is wrong"
    );
    assert_eq!(
        deterministic_polish("he was treated for colon cancer"),
        "He was treated for colon cancer"
    );
    assert_eq!(
        deterministic_polish("add a dash of salt"),
        "Add a dash of salt"
    );
    assert_eq!(
        deterministic_polish("plus we should ship it today"),
        "Plus we should ship it today"
    );
    assert_eq!(
        deterministic_polish("the work was divided by team"),
        "The work was divided by team"
    );
    assert_eq!(
        deterministic_polish("this is a five star review"),
        "This is a 5 star review"
    );
    assert_eq!(
        deterministic_polish("Slash is the guitarist"),
        "Slash is the guitarist"
    );
}

/// The unambiguous spoken forms must still produce their symbols.
#[test]
fn unambiguous_spoken_symbols_still_convert() {
    assert_eq!(deterministic_polish("ship it full stop"), "Ship it.");
    assert_eq!(deterministic_polish("a em dash b"), "A-b");
    assert_eq!(deterministic_polish("bullet asterisk here"), "Bullet*here");
    assert_eq!(deterministic_polish("a forward slash b"), "A/b");
    assert_eq!(deterministic_polish("a plus sign b"), "A+b");
    // Bare `colon` used to run BEFORE this rule and ate it ("semi :").
    assert_eq!(deterministic_polish("a semi colon b"), "A; b");
}

#[test]
fn large_settings_snapshot_reuses_then_replaces_active_set() {
    let cache = OnceLock::new();
    let keys: Vec<String> = (0..5_000).map(|index| format!("term {index}")).collect();
    assert_eq!(regexes_for_keys(&cache, &keys).len(), keys.len());
    assert_eq!(
        cache
            .get()
            .expect("initialized cache")
            .lock()
            .expect("cache mutex")
            .builds,
        1
    );
    assert_eq!(regexes_for_keys(&cache, &keys).len(), keys.len());
    assert_eq!(
        cache
            .get()
            .expect("initialized cache")
            .lock()
            .expect("cache mutex")
            .builds,
        1
    );
    let replacement = vec!["replacement".to_string()];
    assert_eq!(regexes_for_keys(&cache, &replacement).len(), 1);
    let snapshot = cache
        .get()
        .expect("initialized cache")
        .lock()
        .expect("cache mutex");
    assert_eq!(snapshot.keys, replacement);
    assert_eq!(snapshot.regexes.len(), 1);
    assert_eq!(snapshot.builds, 2);
}

#[test]
fn scrub_strips_command_tokens_before_llm() {
    // The reported leak: the streaming committer commits "… prompt, zephyr"
    // before "stop" lands, so it must be scrubbed to nothing here.
    assert_eq!(scrub_command_tokens("prompt, zephyr stop"), "");
    // Trailing bare wake word (verb not committed yet) is removed.
    assert_eq!(
        scrub_command_tokens("ship the report zephyr"),
        "ship the report"
    );
    // Complete control tail is removed, content kept.
    assert_eq!(
        scrub_command_tokens("ship the report zephyr stop"),
        "ship the report"
    );
    // A legitimately dictated bare verb is NOT stripped (no wake word before it).
    assert_eq!(scrub_command_tokens("please send"), "please send");
    // Plain dictation is untouched.
    assert_eq!(
        scrub_command_tokens("the quarterly numbers look good"),
        "the quarterly numbers look good"
    );
    // ASR near-homophone wake variant trailing.
    assert_eq!(scrub_command_tokens("done zeppe"), "done");
}

#[test]
fn parses_zephyr_tail_commands() {
    assert!(parse_control_command("paste this Zephyr submit").is_none());
    let c = parse_control_command("paste this Zephyr send").unwrap();
    assert_eq!(c.clean_text, "paste this");
    assert_eq!(c.intent, ControlIntent::Send);
    let c = parse_control_command("Zephyr stop").unwrap();
    assert_eq!(c.clean_text, "");
    assert_eq!(c.intent, ControlIntent::Stop);
    let c = parse_control_command("paste this zeppe stop").unwrap();
    assert_eq!(c.clean_text, "paste this");
    assert_eq!(c.intent, ControlIntent::Stop);
    let c = parse_control_command("done zeppe send").unwrap();
    assert_eq!(c.clean_text, "done");
    assert_eq!(c.intent, ControlIntent::Send);
    // Parakeet drops the final consonants of "send" -> "sea" at a phrase end
    // (observed 2026-07-01: leaked "zephyr sea" into the delivered text).
    let c = parse_control_command("I just hope it does Zephyr sea").unwrap();
    assert_eq!(c.clean_text, "I just hope it does");
    assert_eq!(c.intent, ControlIntent::Send);
    let c = parse_control_command("done zaffer send").unwrap();
    assert_eq!(c.clean_text, "done");
    assert_eq!(c.intent, ControlIntent::Send);
    let c = parse_control_command("done zapper send").unwrap();
    assert_eq!(c.clean_text, "done");
    assert_eq!(c.intent, ControlIntent::Send);
}

#[test]
fn submit_is_not_a_control_adrian_lock_2026_07_28() {
    for text in [
        "Zephyr submit",
        "done zeppe submit",
        "done zapper submit",
        "ship it zefyr submit",
        "Ready Зэфэр submit",
        "hello stop submit",
        "hello stopsubmit",
    ] {
        assert!(
            parse_control_command(text).is_none(),
            "submit must remain ordinary dictation: {text}"
        );
    }
}

#[test]
fn strip_fired_control_tail_requires_identifiable_trigger_evidence() {
    // A recognized wake + verb is safe to remove.
    assert_eq!(
        strip_fired_control_tail("keep this zephyr stop", ControlIntent::Stop),
        "keep this"
    );
    // If the full-buffer decode rewrites the wake beyond recognition, keep
    // the suffix: it may be real dictated content followed by an omitted
    // trigger, and the fired probe supplies no final-text token boundary.
    assert_eq!(
        strip_fired_control_tail("restored dark hero safe stop", ControlIntent::Stop),
        "restored dark hero safe stop"
    );
    assert_eq!(
        strip_fired_control_tail("crystal dark hero zeppelin stop", ControlIntent::Stop),
        "crystal dark hero zeppelin stop"
    );
    assert_eq!(
        strip_fired_control_tail("ship the report zephon send", ControlIntent::Send),
        "ship the report"
    );
    assert_eq!(
        strip_fired_control_tail("safe stop", ControlIntent::Stop),
        "safe stop"
    );
}

/// Field regression 2026-07-27: the user said "zephyr stop". The probe lane
/// recognized it and stopped the recording, but the full-buffer decode wrote
/// the same audio as "Zephyr s" — wake intact, verb chopped. Neither the
/// full-control parse nor the bare-wake branch matched, so "Zephyr s" was
/// delivered into the user's text.
#[test]
fn strip_fired_control_tail_removes_wake_with_chopped_verb() {
    assert_eq!(
        strip_fired_control_tail("What is broken? Zephyr s", ControlIntent::Stop),
        "What is broken?"
    );
    assert_eq!(
        strip_fired_control_tail("ship the report zephyr sen", ControlIntent::Send),
        "ship the report"
    );
    assert_eq!(
        strip_fired_control_tail("done for now zephyr canc", ControlIntent::Cancel),
        "done for now"
    );
}

/// The chopped-verb path must stay narrow: a wake word followed by real
/// dictation is ordinary speech, not a clipped command.
#[test]
fn chopped_verb_strip_keeps_real_content_after_the_wake_word() {
    for text in [
        "the zephyr constellation is bright tonight",
        "we named the release zephyr because it is fast",
        "zephyr was the codename we picked",
    ] {
        assert_eq!(
            strip_fired_control_tail(text, ControlIntent::Stop),
            text,
            "must not cut: {text}"
        );
    }
}

#[test]
fn strip_fired_control_tail_preserves_content_when_final_asr_omits_trigger() {
    // The streaming probe heard "Zephyr send", but the full-buffer decode
    // omitted/mangled both trigger tokens. Its final two tokens are user
    // content and must never be deleted merely because the probe fired.
    assert_eq!(
        strip_fired_control_tail("please keep these words", ControlIntent::Send),
        "please keep these words"
    );
    assert_eq!(
        strip_fired_control_tail("please press send", ControlIntent::Send),
        "please press send"
    );
}

#[test]
fn strip_fired_control_tail_removes_bare_wake_after_send_fire() {
    // Field regression 2026-07-07: the streaming probe eventually fired on
    // "Zephyr send", but the full-buffer transcript ended with a bare
    // earlier failed trigger fragment ("... Zephyr"). Once the control has
    // fired, that trailing wake token is command residue, not dictation.
    assert_eq!(
            strip_fired_control_tail(
                "And also, what was the issue with the provider timeout? And I don't understand. Zephyr",
                ControlIntent::Send,
            ),
            "And also, what was the issue with the provider timeout? And I don't understand"
        );
    // Do not eat the previous non-wake word when only the final token is the
    // fired command residue.
    assert_eq!(
        strip_fired_control_tail("Please send this after lunch Zephyr", ControlIntent::Send),
        "Please send this after lunch"
    );
    assert_eq!(
        strip_fired_control_tail(
            "And I don't understand. Zephyr Sen Zephyr",
            ControlIntent::Send,
        ),
        "And I don't understand"
    );
}

#[test]
fn strip_fired_control_tail_removes_repeated_failed_attempts() {
    // Raw ASR remains in diagnostics, but every stacked trailing command
    // attempt is control residue and must stay out of final paste.
    assert_eq!(
        strip_fired_control_tail(
            "Sorry, I'm confused. Zephyr Sen Zephyr send",
            ControlIntent::Send,
        ),
        "Sorry, I'm confused"
    );
    assert_eq!(
        strip_fired_control_tail(
            "Testing to see if this works. Zephyr stop. Zephyr stop. Zephyr stop.",
            ControlIntent::Stop,
        ),
        "Testing to see if this works"
    );
}

#[test]
fn kws_control_strip_then_asr_transform_route_keep_separate_authority() {
    for (raw, control, transform, expected_intent, expected_text) in [
        (
            "Compare these builds prompt zephyr stop",
            ControlIntent::Stop,
            "Compare these builds prompt",
            AiTransformIntent::Prompt,
            "Compare these builds",
        ),
        (
            "Summarize what happened summarize zephyr send it",
            ControlIntent::Send,
            "Summarize what happened summarize",
            AiTransformIntent::Summarize,
            "Summarize what happened",
        ),
    ] {
        let after_kws = strip_fired_control_tail(raw, control);
        assert_eq!(after_kws, transform);
        let routed = parse_ai_transform_command(&after_kws).expect("ASR transform route");
        assert_eq!(routed.intent, expected_intent);
        assert_eq!(routed.clean_text, expected_text);
    }
}

#[test]
fn exact_kws_onset_residue_is_removed_before_transform_routing() {
    for (raw, transform, expected_intent, expected_text) in [
        (
            "Compare these builds prompt zeph",
            "Compare these builds prompt",
            AiTransformIntent::Prompt,
            "Compare these builds",
        ),
        (
            "Compare these builds prompt zero",
            "Compare these builds prompt",
            AiTransformIntent::Prompt,
            "Compare these builds",
        ),
        (
            "Meeting notes summarize Z",
            "Meeting notes summarize",
            AiTransformIntent::Summarize,
            "Meeting notes",
        ),
        (
            "Meeting notes summarize Zeb",
            "Meeting notes summarize",
            AiTransformIntent::Summarize,
            "Meeting notes",
        ),
    ] {
        let after_kws = strip_fired_control_tail(raw, ControlIntent::Stop);
        assert_eq!(after_kws, transform);
        let routed = parse_ai_transform_command(&after_kws).expect("ASR transform route");
        assert_eq!(routed.intent, expected_intent);
        assert_eq!(routed.clean_text, expected_text);
    }
}

#[test]
fn exact_kws_onset_residue_cleanup_requires_confirmed_kws_call() {
    for text in [
        "the answer is zero",
        "label this section Z",
        "the abbreviation is ZEP",
    ] {
        assert!(parse_control_command(text).is_none());
        assert!(parse_ai_transform_command(text).is_none());
    }
}

#[test]
fn parses_asr_verb_near_homophones() {
    // Parakeet flips the control verbs to near-homophones at a phrase end.
    let c = parse_control_command("testing this works zephyr sent").unwrap();
    assert_eq!(c.clean_text, "testing this works");
    assert_eq!(c.intent, ControlIntent::Send);
    for phrase in [
        "testing this works zephyr sen",
        "testing this works zephyr sand",
        "testing this works zephyr said",
        "testing this works zephyr says",
    ] {
        let c = parse_control_command(phrase).unwrap();
        assert_eq!(c.clean_text, "testing this works");
        assert_eq!(c.intent, ControlIntent::Send);
    }
    assert_eq!(
        parse_control_command("foo zephyr stopped").unwrap().intent,
        ControlIntent::Stop
    );
    for phrase in ["foo zephyr stuff", "foo zephyr step", "foo zephyr stock"] {
        assert_eq!(
            parse_control_command(phrase).unwrap().intent,
            ControlIntent::Stop
        );
    }
    assert_eq!(
        parse_control_command("foo zephyr cancelled")
            .unwrap()
            .intent,
        ControlIntent::Cancel
    );
    for phrase in ["foo zephyr cansel", "foo zephyr cancle"] {
        assert_eq!(
            parse_control_command(phrase).unwrap().intent,
            ControlIntent::Cancel
        );
    }
    // A bare verb without the wake word stays plain dictation.
    assert!(parse_control_command("i already sent it").is_none());
}

#[test]
fn fuzzy_matches_action_only_after_confirmed_zephyr_wake() {
    for phrase in [
        "keep this zephyr slop",
        "keep this zephyr soft",
        "keep this zephyr stahp",
        "keep this zephyr stab",
    ] {
        let command = parse_control_command(phrase).unwrap();
        assert_eq!(command.clean_text, "keep this");
        assert_eq!(command.intent, ControlIntent::Stop);
    }
    for phrase in ["ship this zephyr and", "ship this zephyr zen"] {
        let command = parse_control_command(phrase).unwrap();
        assert_eq!(command.clean_text, "ship this");
        assert_eq!(command.intent, ControlIntent::Send);
    }
    let cancel = parse_control_command("discard this zephyr cancil").unwrap();
    assert_eq!(cancel.clean_text, "discard this");
    assert_eq!(cancel.intent, ControlIntent::Cancel);

    assert!(parse_control_command("this became soft").is_none());
    assert!(parse_control_command("the zephyr wind").is_none());
    assert!(parse_control_command("ship this zephyr shipment").is_none());
}

#[test]
fn parses_conservative_fuzzy_zephyr_tail_commands() {
    for wake in ["zefyr", "zipper", "zifr", "zeppr"] {
        let c = parse_control_command(&format!("ship it {wake} send")).unwrap();
        assert_eq!(c.clean_text, "ship it");
        assert_eq!(c.wake_word, wake);
        assert_eq!(c.intent, ControlIntent::Send);
    }
    assert!(parse_control_command("ship it zefyr submit").is_none());

    let c = parse_control_command("leave that there zeffir stop").unwrap();
    assert_eq!(c.clean_text, "leave that there");
    assert_eq!(c.wake_word, "zeffir");
    assert_eq!(c.intent, ControlIntent::Stop);

    assert!(parse_control_command("we should stop sending this").is_none());
    assert!(parse_control_command("the word zephyr belongs here").is_none());
    assert!(parse_control_command("ship it safer submit").is_none());
    assert!(parse_control_command("ship it zapper shipment").is_none());
}

#[test]
fn parses_compact_zephyr_tail_commands_emitted_by_coreml() {
    let send = parse_control_command("ship it Zephysend").unwrap();
    assert_eq!(send.clean_text, "ship it");
    assert_eq!(send.intent, ControlIntent::Send);

    let stop = parse_control_command("keep it Zefestop").unwrap();
    assert_eq!(stop.clean_text, "keep it");
    assert_eq!(stop.intent, ControlIntent::Stop);
}

#[test]
fn parses_legacy_stop_tail_commands() {
    let stop = parse_control_command("keep this stop stop").unwrap();
    assert_eq!(stop.clean_text, "keep this");
    assert_eq!(stop.intent, ControlIntent::Stop);
    let c = parse_control_command("hello stop send").unwrap();
    assert_eq!(c.clean_text, "hello");
    assert_eq!(c.intent, ControlIntent::Send);
    let c = parse_control_command("hello stopsend").unwrap();
    assert_eq!(c.clean_text, "hello");
    assert_eq!(c.intent, ControlIntent::Send);
    assert!(parse_control_command("hello stopsubmit").is_none());

    assert!(parse_control_command("we should stop sending this").is_none());
    assert!(parse_control_command("we should stopsending this").is_none());
}

#[test]
fn parses_sapi_command_eval_transcripts() {
    let c = parse_control_command("Please send this to Nidin after lunch Zephyr send.").unwrap();
    assert_eq!(c.clean_text, "Please send this to Nidin after lunch");
    assert_eq!(c.intent, ControlIntent::Send);

    assert!(parse_control_command("The draft looks good to me Zephyr submit.").is_none());

    let c = parse_control_command("I only want to keep the sentence Zephyr stop.").unwrap();
    assert_eq!(c.clean_text, "I only want to keep the sentence");
    assert_eq!(c.intent, ControlIntent::Stop);

    assert!(parse_control_command("The Zephyr wind stopped near the hill.").is_none());
}

#[test]
fn parses_ai_transform_tail_commands() {
    let c = parse_ai_transform_command("make this clearer prompt.").unwrap();
    assert_eq!(c.clean_text, "make this clearer");
    assert_eq!(c.intent, AiTransformIntent::Prompt);
    let c = parse_ai_transform_command("release notes are messy please summarize").unwrap();
    assert_eq!(c.clean_text, "release notes are messy");
    assert_eq!(c.intent, AiTransformIntent::Summarize);
    let c = parse_ai_transform_command("release notes summarize this!").unwrap();
    assert_eq!(c.clean_text, "release notes");
    assert_eq!(c.intent, AiTransformIntent::Summarize);

    assert!(parse_ai_transform_command("prompt").is_none());
    assert!(parse_ai_transform_command("summary").is_none());
    assert!(parse_ai_transform_command("summarized").is_none());
    assert!(parse_ai_transform_command("summarizes").is_none());
    assert!(parse_ai_transform_command("please summarize this tomorrow").is_none());
    assert!(parse_ai_transform_command("make it prompted").is_none());
    assert!(parse_ai_transform_command("make it prompt tomorrow").is_none());
}

#[test]
fn parses_standalone_ai_transform_only_with_selected_text() {
    let summary = parse_selected_text_ai_transform_command(
        "  summarise! ",
        Some(" Priya owns the questionnaire. Send it Friday. "),
    )
    .unwrap();
    assert_eq!(summary.intent, AiTransformIntent::Summarize);
    assert_eq!(
        summary.clean_text,
        "Priya owns the questionnaire. Send it Friday."
    );
    assert_eq!(
        parse_selected_text_ai_transform_command("Summarize.", Some("wall of text"))
            .unwrap()
            .intent,
        AiTransformIntent::Summarize
    );

    // Selection lane is summarize-only: "prompt" on a selection stays
    // ordinary dictation (the L2 prompt intent remains a dictation TAIL).
    assert!(parse_selected_text_ai_transform_command("prompt", Some("selected source")).is_none());
    assert!(parse_selected_text_ai_transform_command("summarize", None).is_none());
    assert!(parse_selected_text_ai_transform_command("summarize", Some("  ")).is_none());
    assert!(parse_selected_text_ai_transform_command("prompt this", Some("source")).is_none());
    assert!(
        parse_selected_text_ai_transform_command("ordinary dictation", Some("source")).is_none()
    );
}

#[test]
fn parses_observed_cyrillic_zephyr_tail_commands() {
    let c = parse_control_command("Ship this Зэфир send").unwrap();
    assert_eq!(c.clean_text, "Ship this");
    assert_eq!(c.intent, ControlIntent::Send);

    let c = parse_control_command("Keep only this Зафир stop").unwrap();
    assert_eq!(c.clean_text, "Keep only this");
    assert_eq!(c.intent, ControlIntent::Stop);

    let c = parse_control_command("Ready Зэфэр send").unwrap();
    assert_eq!(c.clean_text, "Ready");
    assert_eq!(c.intent, ControlIntent::Send);
    assert!(parse_control_command("Ready Зэфэр submit").is_none());
}

#[test]
fn parses_trailing_summarize_transform() {
    let c = parse_ai_transform_command(
        "The launch call covered pricing blockers and model hosting summarize.",
    )
    .unwrap();
    assert_eq!(
        c.clean_text,
        "The launch call covered pricing blockers and model hosting"
    );
    assert_eq!(c.intent, AiTransformIntent::Summarize);
    assert!(parse_ai_transform_command("The provider routing notes summarized.").is_none());
    assert!(parse_ai_transform_command("The provider routing notes summarizes.").is_none());
    assert!(parse_ai_transform_command("The provider routing notes summary.").is_none());
    assert!(parse_ai_transform_command("summarize").is_none());
    assert!(parse_ai_transform_command("Please summarize this tomorrow").is_none());
}

#[test]
fn timed_transform_requires_pause_before_final_trigger() {
    let tokens = vec![
        TimedToken {
            text: "make this clearer".into(),
            start: 0.0,
            end: 1.0,
        },
        TimedToken {
            text: " prompt".into(),
            start: 1.35,
            end: 1.65,
        },
    ];
    assert_eq!(
        parse_timed_ai_transform_command("make this clearer prompt", &tokens)
            .map(|command| command.intent),
        Some(AiTransformIntent::Prompt)
    );
}

#[test]
fn timed_transform_rejects_no_pause_and_literal_trigger() {
    let no_pause = vec![
        TimedToken {
            text: "make this clearer".into(),
            start: 0.0,
            end: 1.0,
        },
        TimedToken {
            text: " prompt".into(),
            start: 1.1,
            end: 1.4,
        },
    ];
    assert!(parse_timed_ai_transform_command("make this clearer prompt", &no_pause).is_none());

    let literal = vec![
        TimedToken {
            text: "use the word".into(),
            start: 0.0,
            end: 0.8,
        },
        TimedToken {
            text: " prompt".into(),
            start: 1.2,
            end: 1.5,
        },
    ];
    assert!(parse_timed_ai_transform_command("Use the word prompt", &literal).is_none());
}

#[test]
fn timed_transform_reconstructs_sentencepiece_trigger_and_checks_only_tail() {
    let pieces = vec![
        TimedToken {
            text: "the".into(),
            start: 0.0,
            end: 0.2,
        },
        TimedToken {
            text: " word".into(),
            start: 0.25,
            end: 0.45,
        },
        TimedToken {
            text: " prompt".into(),
            start: 0.5,
            end: 0.7,
        },
        TimedToken {
            text: " and".into(),
            start: 0.75,
            end: 0.9,
        },
        TimedToken {
            text: " then".into(),
            start: 0.95,
            end: 1.1,
        },
        TimedToken {
            text: " pro".into(),
            start: 1.4,
            end: 1.55,
        },
        TimedToken {
            text: "mpt".into(),
            start: 1.55,
            end: 1.7,
        },
    ];
    assert_eq!(
        parse_timed_ai_transform_command("the word prompt and then prompt", &pieces)
            .map(|command| command.intent),
        Some(AiTransformIntent::Prompt)
    );
}

#[test]
fn timed_transform_does_not_charge_punctuation_time_to_previous_word() {
    let pieces = vec![
        TimedToken {
            text: " happens".into(),
            start: 0.5,
            end: 1.0,
        },
        TimedToken {
            text: ".".into(),
            start: 1.0,
            end: 1.24,
        },
        TimedToken {
            text: " summarize".into(),
            start: 1.24,
            end: 1.7,
        },
    ];
    assert_eq!(
        parse_timed_ai_transform_command("see what happens. summarize", &pieces)
            .map(|command| command.intent),
        Some(AiTransformIntent::Summarize)
    );
}

#[test]
fn formats_inline_macros() {
    assert_eq!(
        apply_inline_formatting("hello new line world"),
        "hello\nworld"
    );
    assert_eq!(
        apply_inline_formatting("hello skip a line world start a new paragraph done"),
        "hello\nworld\n\ndone"
    );
    assert_eq!(apply_inline_formatting("is this question mark"), "is this?");
    assert_eq!(
        apply_inline_formatting(
            "hashtag alpha at sign beta dot com forward slash docs underscore v one percent sign"
        ),
        "#alpha@beta.com/docs_v one%"
    );
    assert_eq!(
        apply_inline_formatting(
            "open angle bracket tag close angle bracket ampersand asterisk equals sign tilde"
        ),
        "<tag>&*=~"
    );
    assert_eq!(
        apply_inline_formatting("less than sign greater than sign degrees c degrees fahrenheit"),
        "<>°C °F"
    );
    assert_eq!(deterministic_polish("hello new line world"), "Hello\nworld");
    // Regression: a newline must survive a SECOND deterministic pass (the polish
    // pipeline runs deterministic_polish before AND after Harper). collapse_repetitions
    // used to flatten \n -> space here, eating spoken "new line"/"new paragraph".
    assert_eq!(
        deterministic_polish(&deterministic_polish("first line new line second line")),
        "First line\nsecond line"
    );
    assert_eq!(collapse_repetitions("a\nb"), "a\nb");
    // Opening brackets need a leading space when glued to the previous char;
    // closing brackets need a trailing space before the next word. Nested
    // brackets stay tight.
    assert_eq!(
        fix_punctuation_spacing("How are you?(coming)"),
        "How are you? (coming)"
    );
    assert_eq!(fix_punctuation_spacing("hi(there)done"), "hi (there) done");
    assert_eq!(fix_punctuation_spacing("see ((nested))"), "see ((nested))");
    assert_eq!(fix_punctuation_spacing("a (b)"), "a (b)");
}

#[test]
fn visual_chunking_accumulates_sentences_to_word_target() {
    assert_eq!(
            visual_chunk_transcript(
                "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty. one two three four five six seven eight nine ten. one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty."
            ),
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty. one two three four five six seven eight nine ten.\none two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty."
        );
    // Existing line breaks remain single line breaks; L0 never invents blank lines.
    assert_eq!(
            visual_chunk_transcript("One two three four five six seven eight nine ten.\nThree four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty.")
                .lines()
                .count(),
            2
        );
}

#[test]
fn visual_chunking_preserves_long_sentences_and_splits_run_ons() {
    let long_sentence = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven twentyeight twentynine thirty thirtyone thirtytwo thirtythree thirtyfour thirtyfive thirtysix thirtyseven thirtyeight thirtynine forty fortyone fortytwo fortythree fortyfour fortyfive fortysix fortyseven fortyeight fortynine fifty fiftyone fiftytwo fiftythree fiftyfour fiftyfive";
    assert_eq!(
        visual_chunk_transcript(&format!("{long_sentence}.")),
        format!("{long_sentence}.")
    );

    let run_on = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven twentyeight twentynine thirty, thirtyone thirtytwo thirtythree thirtyfour thirtyfive thirtysix thirtyseven thirtyeight thirtynine forty fortyone fortytwo fortythree fortyfour fortyfive fortysix fortyseven fortyeight fortynine fifty fiftyone fiftytwo fiftythree fiftyfour fiftyfive fiftysix fiftyseven fiftyeight fiftynine sixty sixtyone sixty-two.";
    let output = visual_chunk_transcript(run_on);
    assert_eq!(output.lines().count(), 2);
    assert_eq!(
        output.lines().next().unwrap().split_whitespace().count(),
        30
    );
    assert!(output.lines().next().unwrap().ends_with(','));
    assert_eq!(output.replace('\n', " "), run_on);
}

#[test]
fn visual_chunking_uses_word_fallback_only_at_whitespace() {
    let input = ("word ".repeat(180) + "tail").trim().to_string();
    let output = visual_chunk_transcript(&input);
    assert!(output.contains('\n'));
    assert!(output.lines().all(|line| !line.ends_with(' ')));
    assert_eq!(output.replace('\n', " "), input);
    assert!(output
        .lines()
        .all(|line| line.split_whitespace().count() <= 40));

    let punctuated = "word. ".repeat(140);
    let output = visual_chunk_transcript(&punctuated);
    assert!(output.lines().all(|line| line.ends_with('.')));
}

#[test]
fn visual_chunking_leaves_commands_paths_and_code_untouched() {
    for text in [
        "git status. git diff. git log. git show.",
        "/Users/adrian/project/src/main.rs. /Users/adrian/project/src/lib.rs.",
        "fn main() { println!(\"one. two. three. four.\"); }",
    ] {
        assert_eq!(visual_chunk_transcript(text), text);
    }
}

#[test]
fn formats_addresses_and_literal_hash_correctly() {
    assert_eq!(deterministic_polish("hashtag fool"), "#fool");
    assert_eq!(deterministic_polish("number sign fool"), "#fool");
    assert_eq!(deterministic_polish("pound sign fool"), "#fool");
    assert_eq!(deterministic_polish("hash browns"), "Hash browns");
    assert_eq!(deterministic_polish("hash map"), "Hash map");
    assert_eq!(deterministic_polish("hash function"), "Hash function");

    assert_eq!(
        deterministic_polish("adrian at gmail dot com"),
        "adrian@gmail.com"
    );
    assert_eq!(deterministic_polish("at gmail dot com"), "@gmail.com");
    assert_eq!(
        deterministic_polish("mail at heardright dot app"),
        "mail@heardright.app"
    );
    assert_eq!(
        deterministic_polish("support at example dot co dot uk"),
        "support@example.co.uk"
    );
    assert_eq!(deterministic_polish("heardright dot app"), "Heardright.app");
    assert_eq!(deterministic_polish("viewright dot cc"), "Viewright.cc");
    assert_eq!(
        deterministic_polish("example dot co dot uk"),
        "Example.co.uk"
    );
    assert_eq!(
        deterministic_polish("company dot co dot in"),
        "Company.co.in"
    );
    assert_eq!(deterministic_polish("meet at five"), "Meet at 5");
}

#[test]
fn cleans_acronyms_and_numbers() {
    assert_eq!(deterministic_polish("tsf and rnnt"), "TSF and RNN-T");
    assert_eq!(
        deterministic_polish("one sixty five commands"),
        "165 commands"
    );
    assert_eq!(
        deterministic_polish("one hundred sixty five commands"),
        "165 commands"
    );
}

#[test]
fn keeps_dictated_wake_words() {
    // Boundary wake-word stripping is OFF until the acoustic wake ships, so a
    // dictated "zephyr" survives (capitalized by the brand-casing rule). Real
    // control tails are stripped earlier by parse_control_command, not here.
    assert_eq!(
        deterministic_polish("zephyr testing the TSF layer"),
        "Zephyr testing the TSF layer"
    );
    assert_eq!(
        deterministic_polish("the word zephyr belongs in this sentence"),
        "The word Zephyr belongs in this sentence"
    );
}

#[test]
fn normalizes_high_value_times_money_and_percent() {
    assert_eq!(
        deterministic_polish("meet me at 915 pm"),
        "Meet me at 9:15 PM"
    );
    assert_eq!(
        deterministic_polish("ship it at four p m"),
        "Ship it at 4:00 PM"
    );
    assert_eq!(deterministic_polish("ten thirty works"), "10:30 works");
    // Cueless bare times only fire on natural minute words (>=10). A small
    // single-digit minute is left as words ("two three" is not "2:03").
    // "two three" is not a time (2:03); with digits-on it's just "2 3".
    assert_eq!(
        deterministic_polish("give me two three options"),
        "Give me 2 3 options"
    );
    assert_eq!(deterministic_polish("meet at two thirty"), "Meet at 2:30");
    // Spoken integers 0-19 render as digits...
    assert_eq!(
        deterministic_polish("i have three apples"),
        "I have 3 apples"
    );
    assert_eq!(deterministic_polish("one two three"), "1 2 3");
    // ...except idiomatic "one" (pronoun), which stays a word.
    assert_eq!(deterministic_polish("no one is here"), "No one is here");
    assert_eq!(deterministic_polish("the one i want"), "The one I want");
    assert_eq!(
        deterministic_polish("one of them broke"),
        "One of them broke"
    );
    // but a counting "one" converts.
    assert_eq!(deterministic_polish("give me one apple"), "Give me 1 apple");
    assert_eq!(deterministic_polish("step one is easy"), "Step 1 is easy");
    assert_eq!(
        deterministic_polish("twenty dollars and five percent"),
        "$20 and 5%"
    );
}

#[test]
fn l0_does_not_delete_discourse_words() {
    use crate::text_pipeline::aggressive_speech_cleanup;
    // L0 used to strip "like", "you know", "i mean" and a leading
    // so/okay/ok/well. All of it was unmeasured, and it made the same
    // sentence come back with different WORDS depending on whether the L1
    // cloud call succeeded — L0 is only reached when L1 is off or fails.
    // Deleting speech is the AI layer's call, with the whole utterance in
    // view. L0 normalises; it does not edit vocabulary.
    assert_eq!(
        aggressive_speech_cleanup("it was, like, a huge deal"),
        "it was, like, a huge deal"
    );
    assert_eq!(
        aggressive_speech_cleanup("Like, we should ship on Friday"),
        "Like, we should ship on Friday"
    );
    assert_eq!(
        aggressive_speech_cleanup("Okay, test number one"),
        "Okay, test number one"
    );
    assert_eq!(
        aggressive_speech_cleanup("we should ship it, you know, today"),
        "we should ship it, you know, today"
    );
}

#[test]
fn inline_scratch_that_retracts_the_clause() {
    use crate::text_pipeline::apply_inline_edits;
    // clause within one sentence
    assert_eq!(
        apply_inline_edits("send the invoice tomorrow scratch that send it Friday"),
        "send it Friday"
    );
    // command retracts the previous full sentence when it stands alone after it
    assert_eq!(
        apply_inline_edits("Send it Tuesday. Scratch that. Send it Monday."),
        "Send it Monday."
    );
    // discourse lead-in ("actually/no/wait/sorry") extends the retraction back one sentence
    assert_eq!(
        apply_inline_edits("Send it Tuesday. Actually, delete that. Send it Monday."),
        "Send it Monday."
    );
    // 'scratch this' variant, unpunctuated mid-utterance (scratch is always a retraction)
    assert_eq!(
        apply_inline_edits("the header is blue scratch this the header is red"),
        "the header is red"
    );
    // 'delete that' with a lowercase continuation is transitive prose — never fires
    assert_eq!(
        apply_inline_edits("please delete that file from the repo"),
        "please delete that file from the repo"
    );
    // multiple retractions in one utterance are all applied (no early exit)
    assert_eq!(
        apply_inline_edits("ship it Friday scratch that ship it Monday scratch that ship it never"),
        "ship it never"
    );
}

#[test]
fn normalizes_dates_units_and_snippets() {
    // No leading "um": hesitation removal moved to N0, at the piece level in
    // `heardright_parakeet_core::detok`, so it happens before any text
    // reaches this layer — and before L1, which is fed raw ASR.
    assert_eq!(
        deterministic_polish("meet me on november fifth"),
        "Meet me on November 5th"
    );
    assert_eq!(
        deterministic_polish("ship it by the twenty first of june 2026"),
        "Ship it by June 21, 2026"
    );
    assert_eq!(
        deterministic_polish("due on june twenty third twenty twenty six"),
        "Due on June 23, 2026"
    );
    assert_eq!(
        deterministic_polish("due on june twenty third two thousand twenty six"),
        "Due on June 23, 2026"
    );
    assert_eq!(
        deterministic_polish("record for five minutes and twenty megabytes"),
        "Record for 5 minutes and 20 MB"
    );
    let snippets = HashMap::from([
        ("email".to_string(), "adrian@example.com".to_string()),
        ("sig".to_string(), "Best,\nAdrian".to_string()),
    ]);

    // L0 no longer deletes discourse words — see
    // `l0_does_not_delete_discourse_words`. It still normalises spacing and
    // punctuation, which is what this line now pins.
    assert_eq!(
        aggressive_speech_cleanup("So, okay, I mean we should ship it, you know, today"),
        "So, okay, I mean we should ship it, you know, today"
    );
    assert_eq!(
        aggressive_speech_cleanup("I like this kind of workflow"),
        "I like this kind of workflow"
    );
    assert_eq!(
        expand_snippets("send slash email then /sig", &snippets),
        "send adrian@example.com then Best,\nAdrian"
    );
}

#[test]
fn normalizes_large_numbers_decimals_and_currency() {
    assert_eq!(
        deterministic_polish("about two million dollars"),
        "About $2,000,000"
    );
    assert_eq!(
        deterministic_polish("fifty thousand users signed up"),
        "50000 users signed up"
    );
    assert_eq!(
        deterministic_polish("revenue rose nine point four percent"),
        "Revenue rose 9.4%"
    );
    assert_eq!(
        deterministic_polish("pi is three point one four"),
        "Pi is 3.14"
    );
    assert_eq!(
        deterministic_polish("we lost two point five million dollars"),
        "We lost $2,500,000"
    );
}

#[test]
fn full_passage_fragment_normalizes() {
    assert_eq!(
        deterministic_polish("revenue rose nine point four percent to about two million dollars"),
        "Revenue rose 9.4% to about $2,000,000"
    );
}

#[test]
fn preserves_existing_small_number_behavior() {
    assert_eq!(deterministic_polish("twenty dollars"), "$20");
    assert_eq!(deterministic_polish("five percent"), "5%");
    assert_eq!(
        deterministic_polish("the room should be set to 22 degrees c with 50 percent humidity"),
        "The room should be set to 22°C with 50% humidity"
    );
    assert_eq!(
        deterministic_polish("please add the hashtag launch review"),
        "Please add the #launch review"
    );
    assert_eq!(
        deterministic_polish("one hundred sixty five commands"),
        "165 commands"
    );
}

#[test]
fn fixes_punctuation_spacing_and_proper_nouns() {
    assert_eq!(
        deterministic_polish_with_brand("adrian ,heard right and new york", Some(&TEST_BRAND)),
        "Adrian, HeardRight and New York"
    );
    assert_eq!(
        deterministic_polish("adrian ,heard right and new york"),
        "Adrian, heard right and New York",
        "no brand repair without host rules"
    );
    assert_eq!(
        deterministic_polish("dji mic mini and rtx 4070 use dml"),
        "DJI Mic Mini and RTX 4070 use DML"
    );
}

#[test]
fn brand_homophone_spares_the_verb_idiom() {
    // The brand: "heard right" preceded by punctuation/article/noun → HeardRight.
    assert_eq!(
        deterministic_polish_with_brand("the heard right app", Some(&TEST_BRAND)),
        "The HeardRight app"
    );
    assert_eq!(
        deterministic_polish_with_brand("adrian ,heard right and new york", Some(&TEST_BRAND)),
        "Adrian, HeardRight and New York"
    );
    // The idiom: "<pronoun> heard right" is the verb — must stay verbatim
    // (whisper_eval clip05 regression). The old unconditional rule corrupted these.
    assert_eq!(
        deterministic_polish_with_brand("i heard right the first time", Some(&TEST_BRAND)),
        "I heard right the first time"
    );
    assert_eq!(
        deterministic_polish_with_brand("you heard right", Some(&TEST_BRAND)),
        "You heard right"
    );
}

#[test]
fn casing_unambiguous_brands() {
    assert_eq!(
        deterministic_polish("we looked at the square space site"),
        "We looked at the Squarespace site"
    );
    // Client-specific names must never be introduced by the suite-wide
    // deterministic defaults. Users can add them through Vocabulary.
    assert!(
        !deterministic_polish("the one click drive footer on youtube and tiktok")
            .contains("OneClickDrive")
    );
    assert_eq!(
        deterministic_polish("adrian works with damned designs rotten hand and stunning strangers"),
        "Adrian works with damned designs rotten hand and stunning strangers"
    );
    assert_eq!(
        deterministic_polish("oracle netflix and huawei use instagram"),
        "Oracle Netflix and Huawei use Instagram"
    );
}

#[test]
fn tail_polish_does_not_capitalize_fragment_start() {
    assert_eq!(deterministic_polish_tail("after lunch"), "after lunch");
    assert_eq!(
        deterministic_polish_tail("after lunch. send it"),
        "after lunch. Send it"
    );
}
