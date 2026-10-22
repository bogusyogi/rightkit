use rightkit_asr::longform::{
    group_cues, render_subtitles, timed_exports, timed_pieces_to_words, timed_pieces_to_words_with,
    LongFormOptions, TimedExports, TimedWord,
};
use rightkit_asr::{Error, StrictLayout, TimedToken, Transducer};
use serde_json::{json, Value};

fn token(text: &str, start: f32, end: f32) -> TimedToken {
    TimedToken {
        text: text.into(),
        start,
        end,
    }
}

fn transcript_type_and_wire() {
    // Assignment in both directions proves identity with exact transcript 0.4.0.
    let shared: rightkit_transcript::TimedWord = TimedWord {
        text: "hi".into(),
        start_ms: 10,
        end_ms: 20,
        source_ms: Some((110, 120)),
    };
    let asr: TimedWord = shared.clone();
    assert_eq!(asr, shared);
    let wire = serde_json::to_value(&asr).unwrap();
    assert_eq!(wire["source_ms"], json!([110, 120]));
    let restored: rightkit_transcript::TimedWord = serde_json::from_value(wire).unwrap();
    assert_eq!(restored, asr);

    let legacy = json!({"text": "hi", "start_ms": 10, "end_ms": 20});
    let parsed: TimedWord = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(parsed.source_ms, None);
    assert_eq!(parsed, TimedWord::new("hi", 10, 20));
    assert_eq!(serde_json::to_value(parsed).unwrap(), legacy);
}

fn words_cues_and_exports() {
    // First sentence of packaged longform_exports.rs pre-transcript golden.
    let pieces = vec![
        token(" Good", 0.0, 0.32),
        token(" morning", 0.40, 0.88),
        token(" everyone.", 0.96, 1.52),
    ];
    let words: Vec<rightkit_transcript::TimedWord> = timed_pieces_to_words(&pieces);
    assert_eq!(
        words,
        vec![
            TimedWord::new("Good", 0, 320),
            TimedWord::new("morning", 400, 880),
            TimedWord::new("everyone.", 960, 1520),
        ]
    );
    assert!(words.iter().all(|word| word.source_ms.is_none()));
    let cues = group_cues(&words);
    assert_eq!(
        cues,
        vec![TimedWord::new("Good morning everyone.", 0, 1520)]
    );
    let expected_srt = "1\n00:00:00,000 --> 00:00:01,520\nGood morning everyone.\n\n";
    let expected_vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:01.520\nGood morning everyone.\n\n";
    let (srt, vtt) = render_subtitles(&cues);
    assert_eq!(srt, expected_srt);
    assert_eq!(vtt, expected_vtt);
    let exports = timed_exports(&pieces);
    assert_eq!(exports.srt.as_deref(), Some(expected_srt));
    assert_eq!(exports.vtt.as_deref(), Some(expected_vtt));
    let timing: Value = serde_json::from_str(exports.json.as_deref().unwrap()).unwrap();
    assert_eq!(timing["segments"].as_array().unwrap().len(), 1);
    assert_eq!(timing["words"].as_array().unwrap().len(), 3);
    assert_eq!(timing["segments"][0]["text"], "Good morning everyone.");
    assert_eq!(timing["segments"][0]["end"], 1.52);
    assert_eq!(timing["words"][1]["start"], 0.4);
    assert_eq!(timed_exports(&[]), TimedExports::default());

    // Public floor contract from packaged whisper_contract.rs.
    let short = vec![token(" hi", 1.0, 1.02), token(" there", 2.0, 2.5)];
    let floored = timed_pieces_to_words(&short);
    assert_eq!(floored[0], TimedWord::new("hi", 1000, 1080));
    assert_eq!(LongFormOptions::default().assemble_words(&short), floored);
    let opts = LongFormOptions {
        word_assembly_floor_ms: None,
        ..LongFormOptions::default()
    };
    let unfloored = timed_pieces_to_words_with(&short, None);
    assert_eq!(unfloored[0], TimedWord::new("hi", 1000, 1020));
    assert_eq!(unfloored[1], TimedWord::new("there", 2000, 2500));
    assert_eq!(opts.assemble_words(&short), unfloored);
}

fn strict_missing_model_refusal() {
    // An executable is a file: no child model directory can exist. Strict
    // layout validates missing mel before config reads or ORT initialization.
    let executable = std::env::current_exe().unwrap();
    assert!(executable.is_file());
    let missing = executable.join("rightkit-asr-missing-model");
    assert!(!missing.exists());
    let error = match Transducer::load_strict(&missing, StrictLayout::PRODUCTION) {
        Ok(_) => panic!("strict load accepted missing model assets"),
        Err(error) => error,
    };
    assert!(
        matches!(error, Error::Config(ref message) if message.contains("strict layout:") && message.contains("mel_nemo.onnx"))
    );
}

fn main() {
    transcript_type_and_wire();
    words_cues_and_exports();
    strict_missing_model_refusal();
    println!("rightkit-asr transcript compatibility & assetless API assertions passed");
}
