//! Typed round trip for every contract the crate validates. A minimal value
//! (every optional field `None`) and a value with every optional field set must
//! serialise to JSON without `null`, pass the crate's own validation and the
//! typed view, and read back to the same value. An explicit `null` in a document
//! must still deserialise to `None`.

#![cfg(feature = "schema")]

use std::collections::BTreeMap;
use std::fmt::Debug;

use rightkit_contracts::types::{
    content_brief as brief, daemon_api as api, delivery_envelope as envelope, demo_plan as plan,
    record_event as event, record_session as session, ContentBrief, DaemonApi, DeliveryEnvelope,
    DemoPlan, RecordEvent, RecordSession, Rect,
};
use rightkit_contracts::{check_typed, validate_str, Contract};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

/// A blake3 content address written with one repeated lowercase hex digit.
fn hash(digit: char) -> String {
    format!("blake3:{}", digit.to_string().repeat(64))
}

/// Serialises the typed value, then checks that the JSON text has no `null`,
/// that the crate validates it (schema, version, semantic rules), that the typed
/// view accepts it, and that it reads back to the same value.
fn assert_round_trip<T>(contract: Contract, value: &T, label: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let text =
        serde_json::to_string(value).unwrap_or_else(|error| panic!("{label}: serialise: {error}"));
    assert!(
        !text.contains("null"),
        "{label}: serialised JSON contains null: {text}"
    );
    validate_str(contract, &text).unwrap_or_else(|error| panic!("{label}: {error}\n{text}"));
    check_typed(contract, &text).unwrap_or_else(|error| panic!("{label}: typed view: {error}"));
    let back: T =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{label}: read back: {error}"));
    assert_eq!(&back, value, "{label}: value changed through JSON");
}

// ---------------------------------------------------------------- content-brief

fn content_brief_minimal() -> ContentBrief {
    ContentBrief {
        contract: "content-brief".into(),
        contract_version: "1.1.0".into(),
        brief_id: "brief.minimal".into(),
        revision: 1,
        supersedes: None,
        rule_revision: None,
        created_at: None,
        brief_type: brief::BriefType::TalkingHead,
        channel: brief::Channel {
            venture: "orthic".into(),
            channel: "main".into(),
            account: "main".into(),
        },
        platforms: vec!["youtube_shorts".into()],
        deliverables: vec![brief::Deliverable::Video],
        hook: None,
        script: None,
        references: Vec::new(),
        facts: Vec::new(),
        review: None,
        stages: vec![brief::Stage {
            id: brief::StageId::Research,
            request_id: "req.research.1".into(),
            input_hash: hash('0'),
            depends_on: Vec::new(),
        }],
        scenario: None,
    }
}

fn content_brief_with_every_optional_set() -> ContentBrief {
    ContentBrief {
        brief_id: "brief.all-optionals".into(),
        revision: 2,
        supersedes: Some(1),
        rule_revision: Some(4),
        created_at: Some("2026-10-08T09:00:00Z".into()),
        brief_type: brief::BriefType::ProductDemo,
        channel: brief::Channel {
            venture: "orthic".into(),
            channel: "cutright-how-to".into(),
            account: "main".into(),
        },
        platforms: vec!["youtube_shorts".into(), "instagram_reels".into()],
        deliverables: vec![brief::Deliverable::Video, brief::Deliverable::Carousel],
        hook: Some("Export a finished cut in two clicks.".into()),
        script: Some("Open the export sheet, then choose a preset.".into()),
        references: vec!["ref.launch-notes".into()],
        facts: vec![brief::Fact {
            fact_id: "fact.export-price-2".into(),
            text: "Export is included in every plan.".into(),
            status: brief::FactStatus::Superseded,
            proof_id: Some("proof.pricing-page".into()),
            superseded_by: Some("fact.export-price-3".into()),
        }],
        review: Some(brief::Review {
            mode: brief::ReviewMode::Manual,
            policy_revision: Some(3),
        }),
        stages: vec![
            brief::Stage {
                id: brief::StageId::Record,
                request_id: "req.record.2".into(),
                input_hash: hash('0'),
                depends_on: Vec::new(),
            },
            brief::Stage {
                id: brief::StageId::Cut,
                request_id: "req.cut.2".into(),
                input_hash: hash('1'),
                depends_on: vec![brief::StageId::Record],
            },
        ],
        scenario: Some(brief::Scenario {
            scenario_id: "export-flow".into(),
            app: brief::ScenarioApp {
                app_id: "cutright".into(),
                bundle_id: Some("com.orthic.cutright".into()),
            },
            runner: brief::Runner {
                host: brief::RunnerHost::MacSecondSession,
            },
            fps: 60,
            targets: vec![brief::Target {
                target_id: "export-button".into(),
                role: "button".into(),
                label: "Export".into(),
            }],
            steps: vec![brief::Step {
                step_id: "open-export".into(),
                label: "Open export sheet".into(),
                action: brief::Action {
                    kind: brief::ActionKind::Click,
                    target_id: Some("export-button".into()),
                    params: Some(json!({ "button": "left" })),
                },
                expect: brief::Expect {
                    postcondition: "export sheet visible".into(),
                    timeout_ms: Some(5000),
                },
                say: Some("Open the export sheet.".into()),
            }],
        }),
        ..content_brief_minimal()
    }
}

#[test]
fn content_brief_minimal_round_trips_without_null() {
    assert_round_trip(
        Contract::ContentBrief,
        &content_brief_minimal(),
        "content-brief minimal",
    );
}

#[test]
fn content_brief_with_every_optional_set_round_trips() {
    assert_round_trip(
        Contract::ContentBrief,
        &content_brief_with_every_optional_set(),
        "content-brief all optionals",
    );
}

// ------------------------------------------------------------ delivery-envelope

fn envelope_media(audio: Option<envelope::Audio>) -> envelope::VideoMedia {
    envelope::VideoMedia {
        hash: hash('2'),
        container: envelope::Container::Mp4,
        codec: envelope::Codec::H264,
        width: 1080,
        height: 1920,
        fps: 30,
        duration_ms: 1000,
        aspect: envelope::Aspect::NineSixteen,
        audio,
    }
}

fn envelope_minimal() -> DeliveryEnvelope {
    DeliveryEnvelope {
        contract: "delivery-envelope".into(),
        contract_version: "1.1.0".into(),
        envelope_id: "env.minimal".into(),
        brief_id: "brief.minimal".into(),
        brief_revision: 1,
        content_revision: 1,
        policy_revision: 1,
        rule_revision: None,
        producer: envelope::Producer {
            engine: envelope::Engine::Cutright,
            engine_version: "1.4.0".into(),
        },
        platforms: vec!["youtube_shorts".into()],
        copy: None,
        payload: envelope::Payload::Video {
            media: envelope_media(None),
            captions: None,
            thumbnail: None,
        },
    }
}

fn envelope_with_every_optional_set() -> DeliveryEnvelope {
    DeliveryEnvelope {
        envelope_id: "env.all-optionals".into(),
        rule_revision: Some(4),
        copy: Some(envelope::EnvelopeCopy {
            caption: Some("Export a finished cut in two clicks.".into()),
            hashtags: vec!["cutright".into()],
            link_placement: Some(envelope::LinkPlacement::Comment),
        }),
        payload: envelope::Payload::Video {
            media: envelope_media(Some(envelope::Audio {
                codec: "aac".into(),
                channels: 2,
                sample_rate: 48_000,
                loudness_lufs: Some(-14.0),
            })),
            captions: Some(envelope::CaptionTrack {
                format: envelope::CaptionFormat::Srt,
                hash: hash('3'),
            }),
            thumbnail: Some(envelope::Still {
                hash: hash('4'),
                mime_type: "image/png".into(),
                width: 1080,
                height: 1920,
                alt: "Export button highlighted in the timeline".into(),
            }),
        },
        ..envelope_minimal()
    }
}

fn envelope_text_payload(locale: Option<String>) -> DeliveryEnvelope {
    DeliveryEnvelope {
        producer: envelope::Producer {
            engine: envelope::Engine::Postright,
            engine_version: "1.0.0".into(),
        },
        payload: envelope::Payload::Text {
            body: "A short text post.".into(),
            locale,
        },
        ..envelope_minimal()
    }
}

fn envelope_carousel_mixed_slide_copy() -> DeliveryEnvelope {
    let still = |digit: char, alt: &str| envelope::Still {
        hash: hash(digit),
        mime_type: "image/webp".into(),
        width: 1080,
        height: 1350,
        alt: alt.into(),
    };
    DeliveryEnvelope {
        producer: envelope::Producer {
            engine: envelope::Engine::Genright,
            engine_version: "1.0.0".into(),
        },
        platforms: vec!["instagram_feed".into()],
        payload: envelope::Payload::Carousel {
            slides: vec![
                envelope::Slide {
                    index: 1,
                    still: still('5', "Cover slide"),
                    slide_copy: None,
                    step_ids: Vec::new(),
                },
                envelope::Slide {
                    index: 2,
                    still: still('6', "Second slide"),
                    slide_copy: Some("Pick a preset.".into()),
                    step_ids: vec!["open-export".into()],
                },
            ],
        },
        ..envelope_minimal()
    }
}

#[test]
fn delivery_envelope_minimal_round_trips_without_null() {
    assert_round_trip(
        Contract::DeliveryEnvelope,
        &envelope_minimal(),
        "delivery-envelope minimal",
    );
}

#[test]
fn delivery_envelope_with_every_optional_set_round_trips() {
    assert_round_trip(
        Contract::DeliveryEnvelope,
        &envelope_with_every_optional_set(),
        "delivery-envelope all optionals",
    );
}

#[test]
fn delivery_envelope_text_payload_without_locale_round_trips() {
    assert_round_trip(
        Contract::DeliveryEnvelope,
        &envelope_text_payload(None),
        "delivery-envelope text, locale absent",
    );
}

#[test]
fn delivery_envelope_text_payload_with_locale_round_trips() {
    assert_round_trip(
        Contract::DeliveryEnvelope,
        &envelope_text_payload(Some("en-GB".into())),
        "delivery-envelope text, locale set",
    );
}

#[test]
fn delivery_envelope_carousel_round_trips_with_mixed_slide_copy() {
    assert_round_trip(
        Contract::DeliveryEnvelope,
        &envelope_carousel_mixed_slide_copy(),
        "delivery-envelope carousel",
    );
}

// --------------------------------------------------------------- record-session

fn file_entry(digit: char, bytes: u64) -> session::FileEntry {
    session::FileEntry {
        blake3: hash(digit),
        bytes,
    }
}

fn session_minimal() -> RecordSession {
    RecordSession {
        contract: "record-session".into(),
        contract_version: "1.2.0".into(),
        bundle_id: "bundle.minimal".into(),
        created_at: "2026-10-08T10:15:00Z".into(),
        status: session::BundleStatus::Complete,
        reason: None,
        recorder: session::Recorder {
            engine: "rightkit-record".into(),
            version: "0.1.0".into(),
            os: session::RecorderOs::Macos,
            os_build: "27.0".into(),
            helper: session::Helper {
                identity: "rightkit-record-helper".into(),
                signed: true,
            },
        },
        scenario: session::ScenarioRef {
            scenario_id: "export-flow".into(),
            blake3: hash('7'),
            steps: 1,
        },
        target: session::Target {
            app_id: "cutright".into(),
            surfaces: vec![session::Surface {
                surface_id: "main".into(),
                kind: session::SurfaceKind::MainWindow,
                parent_surface_id: None,
                native_id: "win-101".into(),
                supported: true,
                unsupported_reason: None,
            }],
        },
        capture: session::Capture {
            fps: 60,
            scale: 2.0,
            codec: "h264".into(),
            width: 2880,
            height: 1800,
            scope: session::CaptureScope::AppSurfaces,
            display_opt_in: false,
            visibility_state: session::VisibilityState::OffScreenVisible,
            border: None,
        },
        geometry_epochs: vec![session::GeometryEpoch {
            epoch: 0,
            start_ms: 0,
            end_ms: None,
            bounds_pt: Rect {
                x: -2000.0,
                y: 100.0,
                width: 1440.0,
                height: 900.0,
            },
            scale: 2.0,
            dpi: None,
        }],
        clock: session::Clock {
            host_clock: session::HostClock::MachAbsoluteTime,
            host_ns_at_start: 8_123_000_000,
            first_accepted_frame: session::FirstFrame {
                frame_index: 0,
                host_ns: 8_123_500_000,
                sample_pts_ns: 0,
            },
            mapping: session::ClockMapping {
                host_to_sample_pts_offset_ns: -8_123_500_000,
                sample_pts_to_encoded_pts_offset_ns: 0,
                timescale: 60_000,
            },
        },
        frames: session::Frames {
            count: 0,
            dropped_count: 0,
        },
        events: session::EventsRef {
            count: 0,
            contract: "record-event".into(),
            contract_version: "1.2.0".into(),
        },
        // The schema requires exactly one of clean.mp4 and clean.mov, so the
        // minimal value sets clean.mp4 and leaves clean.mov absent.
        files: session::Files {
            scenario_json: file_entry('8', 1200),
            events_ndjson: file_entry('9', 9800),
            clock_json: file_entry('a', 310),
            capture_json: file_entry('b', 290),
            clean_mp4: Some(file_entry('c', 48_200_000)),
            clean_mov: None,
        },
        tracks: Vec::new(),
        dependencies: vec![session::Dependency {
            kind: session::DependencyKind::Recipe,
            id: "demo-recipe.product".into(),
            version: "1.2.0".into(),
            blake3: hash('d'),
            bytes: None,
            hotspot: None,
        }],
        redaction: session::Redaction {
            policy_id: "redaction.default".into(),
            policy_version: "1.0.0".into(),
            pixel_masking: session::PixelMasking {
                mode: session::MaskingMode::DeclaredRegions,
                region_count: 0,
            },
        },
        donor_directory_required: false,
    }
}

fn session_with_every_optional_set() -> RecordSession {
    RecordSession {
        // A complete bundle must not carry a reason, so the all-set value is
        // a quarantined bundle, which must name one.
        status: session::BundleStatus::PartialQuarantined,
        reason: Some("capture gap during export".into()),
        target: session::Target {
            app_id: "cutright".into(),
            surfaces: vec![
                session::Surface {
                    surface_id: "main".into(),
                    kind: session::SurfaceKind::MainWindow,
                    parent_surface_id: None,
                    native_id: "win-101".into(),
                    supported: true,
                    unsupported_reason: None,
                },
                session::Surface {
                    surface_id: "export-sheet".into(),
                    kind: session::SurfaceKind::NativeSheet,
                    parent_surface_id: Some("main".into()),
                    native_id: "sheet-7".into(),
                    supported: false,
                    unsupported_reason: Some("sheet capture not available".into()),
                },
            ],
        },
        capture: session::Capture {
            border: Some(session::CaptureBorder {
                required: true,
                access: Some("granted".into()),
                error: Some("border not drawn".into()),
            }),
            ..session_minimal().capture
        },
        geometry_epochs: vec![
            session::GeometryEpoch {
                epoch: 0,
                start_ms: 0,
                end_ms: Some(1000),
                bounds_pt: Rect {
                    x: -2000.0,
                    y: 100.0,
                    width: 1440.0,
                    height: 900.0,
                },
                scale: 2.0,
                dpi: Some(144),
            },
            session::GeometryEpoch {
                epoch: 1,
                start_ms: 1000,
                end_ms: Some(2000),
                bounds_pt: Rect {
                    x: -2000.0,
                    y: 100.0,
                    width: 1280.0,
                    height: 800.0,
                },
                scale: 2.0,
                dpi: Some(144),
            },
        ],
        // The schema allows exactly one of clean.mp4 and clean.mov, so the
        // all-set value uses clean.mov and the minimal value uses clean.mp4.
        files: session::Files {
            clean_mp4: None,
            clean_mov: Some(file_entry('c', 52_400_000)),
            ..session_minimal().files
        },
        tracks: vec![session::Track {
            track_id: "app-audio".into(),
            kind: session::TrackKind::AppAudio,
            file: "tracks/app-audio.m4a".into(),
            blake3: hash('a'),
            bytes: 1_200_000,
            offset_ms: 0,
        }],
        dependencies: vec![session::Dependency {
            kind: session::DependencyKind::CursorAsset,
            id: "cursor.arrow".into(),
            version: "1.0.0".into(),
            blake3: hash('e'),
            bytes: Some(2048),
            hotspot: Some(session::Hotspot { x: 1.0, y: 1.0 }),
        }],
        ..session_minimal()
    }
}

#[test]
fn record_session_minimal_round_trips_without_null() {
    assert_round_trip(
        Contract::RecordSession,
        &session_minimal(),
        "record-session minimal",
    );
}

#[test]
fn record_session_with_every_optional_set_round_trips() {
    assert_round_trip(
        Contract::RecordSession,
        &session_with_every_optional_set(),
        "record-session all optionals",
    );
}

// ---------------------------------------------------------------- record-event

fn event_minimal() -> RecordEvent {
    RecordEvent {
        contract: "record-event".into(),
        contract_version: "1.2.0".into(),
        session_id: "bundle.minimal".into(),
        seq: 0,
        kind: event::EventKind::SessionStart,
        input_kind: None,
        phase: None,
        effect_id: None,
        sub_event_id: None,
        step_index: 0,
        step_id: None,
        host_ns: 0,
        timing: None,
        geometry_epoch: 0,
        dispatch: None,
        pointer: None,
        target: None,
        text: None,
        key: None,
        modifiers: Vec::new(),
        delta: None,
        admitted: None,
        denied: None,
        postcondition: None,
        frame: None,
        frame_dropped: None,
        intruders: Vec::new(),
        redaction_policy: event::PolicyRef {
            policy_id: "redaction.default".into(),
            policy_version: "1.0.0".into(),
        },
    }
}

/// One input event that sets every optional field. The schema constrains
/// kind-specific fields only by presence (`input` needs inputKind, phase,
/// effectId and admitted), so a single event carries every optional shape.
/// Admitted is false, so the `denied` reason the semantic rules require is present.
fn event_with_every_optional_set() -> RecordEvent {
    RecordEvent {
        seq: 7,
        kind: event::EventKind::Input,
        input_kind: Some(event::InputKind::Click),
        phase: Some(event::Phase::Down),
        effect_id: Some("effect.0004".into()),
        sub_event_id: Some("effect.0004.1".into()),
        step_index: 1,
        step_id: Some("open-export".into()),
        host_ns: 8_125_000_000,
        timing: Some(event::Timing {
            admission_ns: Some(8_124_900_000),
            dispatch_ns: Some(8_125_000_000),
            completion_ns: Some(8_125_100_000),
        }),
        dispatch: Some(event::Dispatch {
            space: event::CoordinateSpace::WindowPoints,
            window_bounds_pt: Rect {
                x: 0.0,
                y: 0.0,
                width: 1440.0,
                height: 900.0,
            },
            scale: 2.0,
        }),
        pointer: Some(event::Pointer {
            x_pt: 1210.0,
            y_pt: 42.0,
        }),
        target: Some(event::Target {
            target_id: Some("export-button".into()),
            identity: Some(event::Redacted::Hash {
                digest: hash('e'),
                salt_id: "salt.run-1".into(),
            }),
            role: "button".into(),
            label: Some(event::Redacted::Full {
                value: "Export".into(),
            }),
            rect_pt: Rect {
                x: 1180.0,
                y: 28.0,
                width: 64.0,
                height: 28.0,
            },
        }),
        text: Some(event::Redacted::Omit),
        key: Some("Return".into()),
        modifiers: vec![event::Modifier::Cmd, event::Modifier::Shift],
        delta: Some(event::Delta { x: 0.0, y: 0.0 }),
        admitted: Some(false),
        denied: Some(event::Denied {
            reason: event::DenyReason::NotForegroundSafe,
        }),
        postcondition: Some(event::Postcondition {
            observed: true,
            observed_ns: 8_126_400_000,
            description: "export sheet visible".into(),
        }),
        frame: Some(event::Frame {
            index: 760,
            sample_pts_ns: 12_666_666_666,
        }),
        frame_dropped: Some(event::FrameDropped {
            count: 1,
            interval_ms: 16.7,
            reason: event::DropReason::CaptureGap,
        }),
        intruders: vec![event::Intruder {
            owner: "Finder".into(),
            title: Some("Downloads".into()),
            bounds: Rect {
                x: 900.0,
                y: 80.0,
                width: 500.0,
                height: 400.0,
            },
        }],
        ..event_minimal()
    }
}

#[test]
fn record_event_minimal_round_trips_without_null() {
    assert_round_trip(
        Contract::RecordEvent,
        &event_minimal(),
        "record-event minimal",
    );
}

#[test]
fn record_event_with_every_optional_set_round_trips() {
    assert_round_trip(
        Contract::RecordEvent,
        &event_with_every_optional_set(),
        "record-event all optionals",
    );
}

// ------------------------------------------------------------------ demo-plan

fn plan_minimal() -> DemoPlan {
    DemoPlan {
        contract: "demo-plan".into(),
        contract_version: "1.0.0".into(),
        plan_id: "plan.minimal".into(),
        brief_id: "brief.minimal".into(),
        brief_revision: 1,
        bundle_id: "bundle.minimal".into(),
        bundle_blake3: hash('f'),
        aspects: vec![envelope::Aspect::NineSixteen],
        narration: plan::Narration {
            voice_id: None,
            script_hash: hash('1'),
            duration_ms: 1000,
            words: Vec::new(),
            time_map: vec![plan::Segment::Play {
                source_start_ms: 0,
                source_end_ms: 1000,
                output_start_ms: 0,
                output_end_ms: 1000,
                speed: 1.0,
            }],
        },
        timeline: vec![plan::Action {
            action_id: "cue.minimal".into(),
            primitive: plan::Primitive::AudioCue,
            start: plan::TimeRef::Absolute { absolute_ms: 0 },
            end: None,
            keyframes: Vec::new(),
            params: None,
            safe_region: None,
            provenance: plan::Provenance {
                source: plan::Source::Director,
                locked: false,
                generator_revision: None,
            },
        }],
    }
}

fn plan_with_every_optional_set() -> DemoPlan {
    DemoPlan {
        plan_id: "plan.all-optionals".into(),
        narration: plan::Narration {
            voice_id: Some("voice.default".into()),
            script_hash: hash('1'),
            duration_ms: 9000,
            words: vec![
                plan::Word {
                    word_id: "w.open.1".into(),
                    step_id: "open-export".into(),
                    text: "Open".into(),
                    start_ms: 1200,
                    end_ms: 1450,
                    lexical_confidence: 1.0,
                    timing_confidence: 0.97,
                },
                plan::Word {
                    word_id: "w.open.2".into(),
                    step_id: "open-export".into(),
                    text: "export".into(),
                    start_ms: 1460,
                    end_ms: 1800,
                    lexical_confidence: 1.0,
                    timing_confidence: 0.95,
                },
            ],
            time_map: vec![
                plan::Segment::Play {
                    source_start_ms: 0,
                    source_end_ms: 1000,
                    output_start_ms: 0,
                    output_end_ms: 1000,
                    speed: 1.0,
                },
                plan::Segment::Hold {
                    source_at_ms: 1000,
                    output_start_ms: 1000,
                    output_end_ms: 2000,
                    step_id: Some("open-export".into()),
                },
                plan::Segment::Play {
                    source_start_ms: 1000,
                    source_end_ms: 8000,
                    output_start_ms: 2000,
                    output_end_ms: 9000,
                    speed: 1.0,
                },
            ],
        },
        // The schema allows one anchor field per anchor (maxProperties 1), so
        // the start anchor names a word and the end anchor names a step.
        timeline: vec![plan::Action {
            action_id: "zoom.export-button".into(),
            primitive: plan::Primitive::Transform,
            start: plan::TimeRef::Anchored {
                anchor: plan::Anchor {
                    word_id: Some("w.open.1".into()),
                    ..plan::Anchor::default()
                },
                offset_ms: -200,
            },
            end: Some(plan::TimeRef::Anchored {
                anchor: plan::Anchor {
                    step_id: Some("open-export".into()),
                    ..plan::Anchor::default()
                },
                offset_ms: 1200,
            }),
            keyframes: vec![
                plan::Keyframe {
                    offset_ms: 0,
                    value: json!({ "scale": 1 }),
                },
                plan::Keyframe {
                    offset_ms: 800,
                    value: json!({ "scale": 2.4, "x": 0.82, "y": 0.06 }),
                },
            ],
            params: Some(json!({ "ease": "ease_in_out" })),
            safe_region: Some(plan::SafeRegion::Clear),
            provenance: plan::Provenance {
                source: plan::Source::Director,
                locked: false,
                generator_revision: Some("director.v1".into()),
            },
        }],
        ..plan_minimal()
    }
}

#[test]
fn demo_plan_minimal_round_trips_without_null() {
    assert_round_trip(Contract::DemoPlan, &plan_minimal(), "demo-plan minimal");
}

#[test]
fn demo_plan_with_every_optional_set_round_trips() {
    assert_round_trip(
        Contract::DemoPlan,
        &plan_with_every_optional_set(),
        "demo-plan all optionals",
    );
}

// ----------------------------------------------------------------- daemon-api

fn api_route(
    route_id: &str,
    method: api::Method,
    path: &str,
    mutation: bool,
    responses: Vec<u16>,
) -> api::Route {
    api::Route {
        route_id: route_id.into(),
        method,
        path: path.into(),
        auth_class: api::AuthClassId::DeviceAuthenticatedMutation,
        mutation,
        cas_required: false,
        scopes: vec![api::Scope::Venture],
        responses,
    }
}

fn api_minimal() -> DaemonApi {
    DaemonApi {
        contract: "daemon-api".into(),
        contract_version: "1.0.0".into(),
        api_id: "postright-daemon-minimal".into(),
        transport: api::Transport {
            scheme: "https".into(),
            host: "rightkit-http-listener".into(),
            tls: "required".into(),
        },
        auth_classes: api::AuthClasses {
            device_authenticated_mutation: api::AuthClass {
                credentials: vec!["device_token".into()],
                mutation_allowed: true,
                required_headers: vec!["Authorization".into()],
                summary: "Device-token requests.".into(),
            },
            oauth_callback: api::AuthClass {
                credentials: vec!["oauth_state_pkce".into()],
                mutation_allowed: false,
                required_headers: Vec::new(),
                summary: "Provider redirect with one-time state.".into(),
            },
            signed_webhook: api::AuthClass {
                credentials: vec!["hmac_sha256".into()],
                mutation_allowed: false,
                required_headers: vec!["X-Provider-Signature".into()],
                summary: "Provider delivery verified by HMAC.".into(),
            },
        },
        // The upload routes must name declared routes, so all four are declared.
        routes: vec![
            api_route(
                "upload.init",
                api::Method::Post,
                "/v1/uploads",
                true,
                vec![201, 400, 401],
            ),
            api_route(
                "upload.chunk",
                api::Method::Put,
                "/v1/uploads/{uploadId}/chunks/{index}",
                true,
                vec![204, 400, 401],
            ),
            api_route(
                "upload.status",
                api::Method::Get,
                "/v1/uploads/{uploadId}",
                false,
                vec![200, 401],
            ),
            api_route(
                "upload.complete",
                api::Method::Post,
                "/v1/uploads/{uploadId}/complete",
                true,
                vec![200, 400, 401],
            ),
        ],
        upload: api::Upload {
            protocol: "resumable-blake3".into(),
            digest: api::Digest {
                algorithm: "blake3".into(),
                mode: "keyed".into(),
                key_scope: "upload-session".into(),
            },
            chunk_bytes: api::ChunkBytes {
                min: 1_048_576,
                max: 16_777_216,
            },
            max_object_bytes: 8_589_934_592,
            session_ttl_seconds: 86_400,
            durable_ack_required: true,
            routes: api::UploadRoutes {
                init: "upload.init".into(),
                chunk: "upload.chunk".into(),
                status: "upload.status".into(),
                complete: "upload.complete".into(),
            },
        },
        mutations: api::Mutations {
            protocol: "compare-and-swap".into(),
            precondition: api::Precondition {
                header: "If-Match".into(),
                revision_type: "integer".into(),
            },
            conflict: api::Conflict {
                status: 409,
                returns: vec!["currentRevision".into()],
            },
            idempotency: api::Idempotency {
                header: "Idempotency-Key".into(),
                ttl_seconds: 86_400,
            },
        },
        errors: vec![api::ErrorEntry {
            code: "unauthenticated".into(),
            status: 401,
            retryable: false,
        }],
        lifecycle_events: vec!["daemon_started".into()],
        exchanges: Vec::new(),
    }
}

fn api_with_every_optional_set() -> DaemonApi {
    let mut headers = BTreeMap::new();
    headers.insert("Idempotency-Key".to_string(), "upload-init-1".to_string());
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    DaemonApi {
        api_id: "postright-daemon-all-optionals".into(),
        exchanges: vec![api::Exchange {
            route_id: "upload.init".into(),
            request: api::ExchangeRequest {
                headers,
                body: Some(json!({ "fileName": "clip.mp4", "bytes": 1024 })),
            },
            response: api::ExchangeResponse {
                status: 201,
                body: Some(json!({ "uploadId": "up.1" })),
            },
        }],
        ..api_minimal()
    }
}

#[test]
fn daemon_api_minimal_round_trips_without_null() {
    assert_round_trip(Contract::DaemonApi, &api_minimal(), "daemon-api minimal");
}

#[test]
fn daemon_api_with_every_optional_set_round_trips() {
    assert_round_trip(
        Contract::DaemonApi,
        &api_with_every_optional_set(),
        "daemon-api all optionals",
    );
}

// ------------------------------------------------- explicit null still accepted

#[test]
fn explicit_null_for_an_optional_field_still_reads_as_none() {
    let mut brief_json = serde_json::to_value(content_brief_minimal()).expect("serialises");
    for key in [
        "supersedes",
        "ruleRevision",
        "createdAt",
        "hook",
        "script",
        "review",
        "scenario",
    ] {
        brief_json[key] = Value::Null;
    }
    let brief: ContentBrief =
        serde_json::from_value(brief_json).expect("explicit nulls deserialise");
    assert_eq!(brief, content_brief_minimal());

    let mut envelope_json = serde_json::to_value(envelope_minimal()).expect("serialises");
    envelope_json["ruleRevision"] = Value::Null;
    envelope_json["copy"] = Value::Null;
    envelope_json["payload"]["captions"] = Value::Null;
    envelope_json["payload"]["thumbnail"] = Value::Null;
    envelope_json["payload"]["media"]["audio"] = Value::Null;
    let envelope: DeliveryEnvelope =
        serde_json::from_value(envelope_json).expect("explicit nulls deserialise");
    assert_eq!(envelope, envelope_minimal());
}
