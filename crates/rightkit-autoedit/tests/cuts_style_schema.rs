//! Style schema scenarios (#93, #94): presets serialise with provenance, every
//! value carries a measured / model-labelled / authored flag, and values map
//! onto the cut configuration they came from.

use rightkit_autoedit::cuts::{
    CutConfig, EditingStyle, FillerPolicy, GapMs, Pace, Preset, Provenance, StyleValue,
};

const PRESETS: [Preset; 3] = [Preset::Tight, Preset::Natural, Preset::Punchy];

fn all_provenances(s: &EditingStyle) -> Vec<&Provenance> {
    vec![
        &s.max_pause.provenance,
        &s.sentence_pause.provenance,
        &s.release_pad.provenance,
        &s.attack_pad.provenance,
        &s.lead_in.provenance,
        &s.tail_out.provenance,
        &s.min_cut.provenance,
        &s.filler_isolation.provenance,
        &s.fillers.provenance,
    ]
}

#[test]
fn presets_round_trip_with_provenance() {
    for p in PRESETS {
        let style = EditingStyle::from_preset(p);
        let json = serde_json::to_string(&style).unwrap();
        let back: EditingStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, style, "{p:?} must round trip");
        assert_eq!(
            serde_json::to_string(&back).unwrap(),
            json,
            "{p:?} serialises stably"
        );
        assert!(
            json.contains("\"kind\":\"authored\""),
            "{p:?} provenance is serialised"
        );
        assert!(
            !json.contains("\"pace\""),
            "{p:?} has no pace value, so none is written"
        );
    }
}

#[test]
fn every_preset_value_is_flagged_authored_not_measured_or_model_labelled() {
    for p in PRESETS {
        let style = EditingStyle::from_preset(p);
        for prov in all_provenances(&style) {
            match prov {
                Provenance::Authored { source } => {
                    assert!(
                        source.starts_with("CutConfig::"),
                        "{p:?} source names the config: {source}"
                    );
                }
                other => panic!("{p:?} expected authored provenance, got {other:?}"),
            }
        }
        assert!(style.pace.is_none(), "{p:?} records no measured pace");
    }
}

#[test]
fn preset_values_match_the_cut_configuration_they_come_from() {
    for p in PRESETS {
        let cfg = CutConfig::for_preset(p);
        let style = EditingStyle::from_preset(p);
        assert_eq!(style.preset, p);
        assert_eq!(style.max_pause.value, GapMs(cfg.max_pause_ms));
        assert_eq!(style.sentence_pause.value, GapMs(cfg.sentence_pause_ms));
        assert_eq!(style.release_pad.value, GapMs(cfg.release_pad_ms));
        assert_eq!(style.attack_pad.value, GapMs(cfg.attack_pad_ms));
        assert_eq!(style.lead_in.value, GapMs(cfg.lead_in_ms));
        assert_eq!(style.tail_out.value, GapMs(cfg.tail_out_ms));
        assert_eq!(style.min_cut.value, GapMs(cfg.min_cut_ms));
        assert_eq!(style.filler_isolation.value, GapMs(cfg.filler_isolation_ms));
        let expected = if cfg.remove_fillers {
            FillerPolicy::Remove
        } else {
            FillerPolicy::Keep
        };
        assert_eq!(style.fillers.value, expected);
    }
    assert_eq!(
        EditingStyle::from_preset(Preset::Punchy).lead_in.value,
        GapMs(40)
    );
    assert_eq!(
        EditingStyle::from_preset(Preset::Tight).max_pause.value,
        GapMs(200)
    );
}

#[test]
fn gaps_serialise_as_bare_milliseconds_and_fillers_as_snake_case() {
    assert_eq!(serde_json::to_string(&GapMs(200)).unwrap(), "200");
    assert_eq!(serde_json::to_string(&Pace(150)).unwrap(), "150");
    assert_eq!(
        serde_json::to_string(&FillerPolicy::Remove).unwrap(),
        "\"remove\""
    );
}

#[test]
fn measured_value_round_trips_with_method_and_source_hash() {
    let source = b"footage-silence-map-bytes";
    let value = StyleValue {
        value: GapMs(180),
        provenance: Provenance::measured("silence-gap-median-v1", source),
    };
    let json = serde_json::to_string(&value).unwrap();
    assert!(json.contains("\"kind\":\"measured\""));
    let back: StyleValue<GapMs> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, value);
    match back.provenance {
        Provenance::Measured {
            method,
            source_hash,
        } => {
            assert_eq!(method, "silence-gap-median-v1");
            assert_eq!(source_hash, blake3::hash(source).to_hex().to_string());
        }
        other => panic!("expected measured provenance, got {other:?}"),
    }
}

#[test]
fn model_labelled_value_round_trips_with_model_id() {
    let value = StyleValue {
        value: FillerPolicy::Keep,
        provenance: Provenance::model_labelled("example-model-id"),
    };
    let json = serde_json::to_string(&value).unwrap();
    assert!(json.contains("\"kind\":\"model_labelled\""));
    assert!(json.contains("example-model-id"));
    let back: StyleValue<FillerPolicy> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, value);
}

#[test]
fn measured_pace_round_trips_inside_a_style() {
    let mut style = EditingStyle::from_preset(Preset::Natural);
    style.pace = Some(StyleValue {
        value: Pace(150),
        provenance: Provenance::measured("words-per-minute-v1", b"transcript-bytes"),
    });
    let json = serde_json::to_string(&style).unwrap();
    assert!(json.contains("\"pace\""));
    let back: EditingStyle = serde_json::from_str(&json).unwrap();
    assert_eq!(back, style);
}
