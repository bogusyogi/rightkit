//! Model-free contract checks for the 0.2.0 surface: option defaults (unchanged
//! behaviour), language validation, sample arithmetic, the word-assembly floor
//! and the AudioSource hook. No model or native library is needed.

use rightkit_asr::longform::{self, LongFormOptions, DEFAULT_WORD_ASSEMBLY_FLOOR_MS};
use rightkit_asr::{
    is_whisper_language, samples_to_ms, validate_language, AudioSource, BeamOptions, DecodeMode,
    Language, TimedToken, WhisperOptions, WHISPER_LANGUAGES, WHISPER_SAMPLE_RATE,
};

#[test]
fn whisper_defaults_keep_0_1_0_behaviour() {
    let o = WhisperOptions::default();
    assert_eq!(o.language, Language::Auto);
    assert_eq!(o.mode, DecodeMode::Timed);
    assert!(!o.token_timestamps, "word timing is opt-in");
    assert_eq!(o.max_len, 0);
    assert!(!o.split_on_word);
    assert!(!o.detect_language);
    assert_eq!(o.threads, 0);
    assert_eq!(o.window_deadline, Some(std::time::Duration::from_secs(180)));
}

#[test]
fn language_table_is_100_codes_with_yue() {
    assert_eq!(WHISPER_LANGUAGES.len(), 100);
    assert!(is_whisper_language("yue"));
    assert!(is_whisper_language(" EN "));
    assert!(!is_whisper_language("english"));
    assert_eq!(validate_language("JA").unwrap(), "ja");
    assert!(validate_language("xx").is_err());
}

#[test]
fn language_validated_maps_auto_and_rejects_unknown_codes() {
    assert_eq!(Language::validated("").unwrap(), Language::Auto);
    assert_eq!(Language::validated("AUTO").unwrap(), Language::Auto);
    assert_eq!(Language::validated("Yue").unwrap(), Language::Code("yue".into()));
    assert!(Language::validated("klingon").is_err());
    // The lenient 0.1.0 parser is unchanged and does not validate.
    assert_eq!(Language::parse("klingon"), Language::Code("klingon".into()));
}

#[test]
fn sample_arithmetic_is_exact_integer() {
    assert_eq!(WHISPER_SAMPLE_RATE, 16_000);
    assert_eq!(samples_to_ms(16_000, 16_000), 1_000);
    assert_eq!(samples_to_ms(160, 16_000), 10, "one centisecond is 160 samples");
    assert_eq!(samples_to_ms(9_120, 16_000), 570, "cs 57 -> 9120 samples -> 570 ms");
    assert_eq!(samples_to_ms(1, 16_000), 0, "rounds down");
    assert_eq!(samples_to_ms(5, 0), 0, "zero rate is guarded");
    // Large values stay exact (no f32 round-trip): one hour at 16 kHz.
    assert_eq!(samples_to_ms(16_000 * 3_600, 16_000), 3_600_000);
}

#[test]
fn audio_source_variants_construct() {
    let s = AudioSource::Pcm16k(vec![0.0; 4]);
    match s {
        AudioSource::Pcm16k(v) => assert_eq!(v.len(), 4),
        AudioSource::Wav(_) => panic!("wrong variant"),
    }
    let w = AudioSource::Wav("x.wav".into());
    assert!(matches!(w, AudioSource::Wav(_)));
}

#[test]
fn beam_defaults_are_unchanged_and_documented_knobs_exist() {
    let b = BeamOptions::default();
    assert_eq!(b.max_symbols, 10, "beam path default stays 10");
    assert!(!b.frames_only_ends, "duration-based ends stay the default");
}

fn tok(text: &str, start: f32, end: f32) -> TimedToken {
    TimedToken { text: text.into(), start, end }
}

#[test]
fn word_floor_default_matches_0_1_0_and_none_removes_it() {
    assert_eq!(DEFAULT_WORD_ASSEMBLY_FLOOR_MS, 80);
    assert_eq!(LongFormOptions::default().word_assembly_floor_ms, Some(80));

    // A one-frame word whose model end is 20 ms after its start.
    let pieces = vec![tok(" hi", 1.0, 1.02), tok(" there", 2.0, 2.5)];
    let with_floor = longform::timed_pieces_to_words(&pieces);
    assert_eq!(with_floor[0].start_ms, 1000);
    assert_eq!(with_floor[0].end_ms, 1080, "floored to start + 80 ms");

    let none = longform::timed_pieces_to_words_with(&pieces, None);
    assert_eq!(none[0].end_ms, 1020, "no floor: the model end is kept");
    assert_eq!(none[1].end_ms, 2500);

    // The options method uses the policy's floor.
    let opts = LongFormOptions { word_assembly_floor_ms: None, ..LongFormOptions::default() };
    assert_eq!(opts.assemble_words(&pieces)[0].end_ms, 1020);
    assert_eq!(LongFormOptions::default().assemble_words(&pieces), with_floor);
}

#[test]
fn word_floor_wins_over_an_earlier_next_word_start() {
    // Next word starts at 1.04 s, before start + 80 ms = 1.08 s. The end is
    // max(next start, floor), so the floor wins (the documented 0.1.0 clamp).
    let pieces = vec![tok(" a", 1.0, 1.0), tok(" b", 1.04, 1.1)];
    let words = longform::timed_pieces_to_words_with(&pieces, Some(80));
    assert_eq!(words[0].end_ms, 1080);
    // Without the floor the model's own end (1.00 s) is kept: the next start only clamps it down.
    let none = longform::timed_pieces_to_words_with(&pieces, None);
    assert_eq!(none[0].end_ms, 1000);
}
