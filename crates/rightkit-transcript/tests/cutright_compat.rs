//! CutRight `cutright.transcript.v1` compatibility: a CutRight-shaped project
//! file (with `source_boundary` ends) reads strictly and leniently, round-trips,
//! and keeps the wire shape CutRight's `deny_unknown_fields` reader expects.
use rightkit_transcript::*;
use serde_json::{json, Value};

/// Shaped like a file CutRight writes: the same field set at every level,
/// including a word clamped to the media boundary.
const CUTRIGHT_FIXTURE: &str = r#"{
  "schema": "cutright.transcript.v1",
  "source_blake3": "ab12",
  "language": "en",
  "duration_ms": 1000,
  "words": [
    {"text": "hello", "start_ms": 0, "end_ms": 400, "confidence": 0.91, "end_source": "model"},
    {"text": "world", "start_ms": 500, "end_ms": 1000, "end_source": "source_boundary"}
  ],
  "vad": [{"start_ms": 0, "end_ms": 1000}],
  "model": {"id": "parakeet-tdt-0.6b-v3-coreml", "revision": "r1", "backend": "coreml"}
}"#;

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[test]
fn cutright_fixture_reads_strict_and_lenient_identically() {
    let lenient = Transcript::from_json(CUTRIGHT_FIXTURE).unwrap();
    let strict = Transcript::from_json_strict(CUTRIGHT_FIXTURE).unwrap();
    assert_eq!(lenient, strict);
    assert_eq!(lenient.schema, LEGACY_SCHEMA);
    assert_eq!(lenient.words[1].end_source, EndSource::SourceBoundary);
    assert_eq!(lenient.words[0].confidence, Some(0.91));
}

#[test]
fn cutright_fixture_round_trips_with_source_boundary() {
    let original = Transcript::from_json(CUTRIGHT_FIXTURE).unwrap();
    let written = original.to_json();
    let back = Transcript::from_json_strict(&written).unwrap();
    assert_eq!(back, original);
    let v: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(v["words"][1]["end_source"], json!("source_boundary"));
}

#[test]
fn rightkit_output_with_legacy_id_keeps_cutright_wire_shape() {
    let original = Transcript::from_json(CUTRIGHT_FIXTURE).unwrap();
    let out = original.to_json_with_schema_id(LEGACY_SCHEMA).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["schema"], json!("cutright.transcript.v1"));
    // The exact key sets CutRight's deny_unknown_fields structs declare.
    assert_eq!(
        keys(&v),
        vec![
            "duration_ms",
            "language",
            "model",
            "schema",
            "source_blake3",
            "vad",
            "words"
        ]
    );
    assert_eq!(
        keys(&v["words"][0]),
        vec!["confidence", "end_ms", "end_source", "start_ms", "text"]
    );
    assert_eq!(keys(&v["vad"][0]), vec!["end_ms", "start_ms"]);
    assert_eq!(keys(&v["model"]), vec!["backend", "id", "revision"]);
    // The legacy-id output reads back to the same transcript.
    assert_eq!(Transcript::from_json_strict(&out).unwrap(), original);
}

#[test]
fn constrained_cutright_transcript_round_trips() {
    let mut t = Transcript::from_json(CUTRIGHT_FIXTURE).unwrap();
    t.constrain_duration(800).unwrap();
    assert_eq!(t.duration_ms, 800);
    assert_eq!(t.words[1].end_ms, 800);
    assert_eq!(t.words[1].end_source, EndSource::SourceBoundary);
    assert_eq!(
        t.vad,
        vec![Span {
            start_ms: 0,
            end_ms: 800
        }]
    );
    let back = Transcript::from_json_strict(&t.to_json()).unwrap();
    assert_eq!(back, t);
}

#[test]
fn strict_mirror_matches_public_types_for_a_full_transcript() {
    let t = Transcript {
        schema: SCHEMA.into(),
        source_blake3: "cd".into(),
        language: "en".into(),
        duration_ms: 3000,
        words: vec![
            Word {
                text: "a".into(),
                start_ms: 0,
                end_ms: 100,
                confidence: Some(0.5),
                end_source: EndSource::Vad,
            },
            Word {
                text: "b".into(),
                start_ms: 200,
                end_ms: 300,
                confidence: None,
                end_source: EndSource::Interpolated,
            },
            Word {
                text: "c".into(),
                start_ms: 400,
                end_ms: 500,
                confidence: None,
                end_source: EndSource::SourceBoundary,
            },
        ],
        vad: vec![Span {
            start_ms: 0,
            end_ms: 600,
        }],
        model: ModelRef {
            id: "m".into(),
            revision: "r".into(),
            backend: "onnx-cpu".into(),
        },
    };
    let s = t.to_json();
    assert_eq!(Transcript::from_json_strict(&s).unwrap(), t);
    assert_eq!(Transcript::from_json(&s).unwrap(), t);
}

#[test]
fn strict_rejects_unknown_fields_at_every_level_while_lenient_ignores_them() {
    let base: Value = serde_json::from_str(CUTRIGHT_FIXTURE).unwrap();
    let cases: Vec<(&str, Value)> = vec![
        ("top level", {
            let mut v = base.clone();
            v["extra"] = json!(1);
            v
        }),
        ("word", {
            let mut v = base.clone();
            v["words"][0]["extra"] = json!(1);
            v
        }),
        ("vad span", {
            let mut v = base.clone();
            v["vad"][0]["extra"] = json!(1);
            v
        }),
        ("model", {
            let mut v = base.clone();
            v["model"]["extra"] = json!(1);
            v
        }),
    ];
    for (level, value) in cases {
        let text = value.to_string();
        match Transcript::from_json_strict(&text) {
            Err(TranscriptError::Json(msg)) => {
                assert!(msg.contains("unknown field"), "{level}: {msg}")
            }
            other => panic!("{level}: expected unknown-field error, got {other:?}"),
        }
        assert!(
            Transcript::from_json(&text).is_ok(),
            "{level}: lenient read must ignore the field"
        );
    }
}

#[test]
fn strict_still_rejects_bad_values_through_validation() {
    let bad = CUTRIGHT_FIXTURE.replace("\"start_ms\": 500", "\"start_ms\": 300");
    assert_eq!(
        Transcript::from_json_strict(&bad),
        Err(TranscriptError::Unsorted { index: 1 })
    );
    let unknown_end = CUTRIGHT_FIXTURE.replace("source_boundary", "clamped_somewhere");
    assert!(matches!(
        Transcript::from_json_strict(&unknown_end),
        Err(TranscriptError::Json(_))
    ));
}

#[test]
fn only_readable_schema_ids_are_accepted_on_read_and_write() {
    assert_eq!(ACCEPTED_SCHEMAS, &[SCHEMA, LEGACY_SCHEMA]);
    let t = Transcript::from_json(CUTRIGHT_FIXTURE).unwrap();
    assert!(t.to_json_with_schema_id(SCHEMA).is_ok());
    assert!(t.to_json_with_schema_id(LEGACY_SCHEMA).is_ok());
    assert_eq!(
        t.to_json_with_schema_id("rightkit.transcript.v2"),
        Err(TranscriptError::Schema("rightkit.transcript.v2".into()))
    );
    let unknown = CUTRIGHT_FIXTURE.replace("cutright.transcript.v1", "older.transcript.v0");
    assert_eq!(
        Transcript::from_json(&unknown),
        Err(TranscriptError::Schema("older.transcript.v0".into()))
    );
    assert_eq!(
        Transcript::from_json_strict(&unknown),
        Err(TranscriptError::Schema("older.transcript.v0".into()))
    );
}

#[test]
fn default_writer_output_is_unchanged_for_existing_values() {
    let t = Transcript::from_words(
        &[
            TimedWord::new("hi", 0, 400),
            TimedWord::new("there", 500, 900),
        ],
        "en",
        Some(1000),
        ModelRef {
            id: "m".into(),
            revision: "r".into(),
            backend: "coreml".into(),
        },
        EndSource::Interpolated,
    );
    let got: Value = serde_json::from_str(&t.to_json()).unwrap();
    assert_eq!(
        got,
        json!({
            "schema": "rightkit.transcript.v1",
            "source_blake3": "",
            "language": "en",
            "duration_ms": 1000,
            "words": [
                {"text": "hi", "start_ms": 0, "end_ms": 400, "end_source": "interpolated"},
                {"text": "there", "start_ms": 500, "end_ms": 900, "end_source": "interpolated"}
            ],
            "vad": [],
            "model": {"id": "m", "revision": "r", "backend": "coreml"}
        })
    );
    assert_eq!(
        serde_json::to_string(&EndSource::Model).unwrap(),
        "\"model\""
    );
}

#[test]
fn strict_requires_each_cutright_field_by_name_while_lenient_defaults_it() {
    type Mutation = fn(&mut Value);
    let base: Value = serde_json::from_str(CUTRIGHT_FIXTURE).unwrap();
    let cases: [(&str, Mutation); 7] = [
        ("source_blake3", |v| {
            v.as_object_mut().unwrap().remove("source_blake3");
        }),
        ("language", |v| {
            v.as_object_mut().unwrap().remove("language");
        }),
        ("vad", |v| {
            v.as_object_mut().unwrap().remove("vad");
        }),
        ("model", |v| {
            v.as_object_mut().unwrap().remove("model");
        }),
        ("model.id", |v| {
            v["model"].as_object_mut().unwrap().remove("id");
        }),
        ("model.revision", |v| {
            v["model"].as_object_mut().unwrap().remove("revision");
        }),
        ("model.backend", |v| {
            v["model"].as_object_mut().unwrap().remove("backend");
        }),
    ];
    for (field, strip) in cases {
        let mut v = base.clone();
        strip(&mut v);
        let text = v.to_string();
        assert_eq!(
            Transcript::from_json_strict(&text),
            Err(TranscriptError::MissingField { field }),
            "strict must name {field}"
        );
        let lenient = Transcript::from_json(&text)
            .unwrap_or_else(|e| panic!("lenient must accept missing {field}: {e:?}"));
        assert_eq!(lenient.words.len(), 2);
    }
}

#[test]
fn strict_rejects_empty_source_blake3_but_lenient_accepts_it() {
    let text = CUTRIGHT_FIXTURE.replace("\"source_blake3\": \"ab12\"", "\"source_blake3\": \"\"");
    assert_eq!(
        Transcript::from_json_strict(&text),
        Err(TranscriptError::MissingField {
            field: "source_blake3"
        })
    );
    assert!(Transcript::from_json(&text).is_ok());
}

#[test]
fn coordinator_minimal_document_is_refused_by_strict_and_defaulted_by_lenient() {
    // The reported regression: schema and words only. Strict must not accept it.
    let minimal = r#"{"schema":"cutright.transcript.v1","duration_ms":0,"words":[]}"#;
    assert_eq!(
        Transcript::from_json_strict(minimal),
        Err(TranscriptError::MissingField {
            field: "source_blake3"
        })
    );
    let lenient = Transcript::from_json(minimal).unwrap();
    assert_eq!(lenient.source_blake3, "");
    assert_eq!(lenient.model, ModelRef::default());
    assert!(lenient.vad.is_empty());
}

#[test]
fn validate_accepts_both_schema_ids_on_purpose() {
    let mut t = Transcript::from_json(CUTRIGHT_FIXTURE).unwrap();
    assert_eq!(t.schema, LEGACY_SCHEMA);
    assert!(t.validate().is_ok());
    t.schema = SCHEMA.into();
    assert!(t.validate().is_ok());
}
