//! Reader additions: `open_complete`, `scenario`, `events`, `clock_map`,
//! `frame_at_target` and `cursor_assets`, on bundles the recorder sealed through
//! the fake backend.

mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use common::{blank_event, make_writable, read_manifest, recorder, request, Scratch};
use rightkit_contracts::types::record_event::{EventKind, Postcondition, RecordEvent, Target};
use rightkit_contracts::types::record_session::BundleStatus;
use rightkit_contracts::types::Rect;
use rightkit_record::fake::{frame, FakeBackend};
use rightkit_record::{
    BundleReader, Code, CursorAssetsStatus, HelperEvent, ScenarioSpec, ScenarioStep, VerifyOptions,
};
use serde_json::{json, Value};

const T0: u64 = 1_000_000;
const FRAME_NS: u64 = 16_666_667;
const FIRST_HOST: u64 = 5_000_000;

fn frames_batch(n: u64) -> Vec<HelperEvent> {
    (0..n)
        .map(|i| {
            let sample = frame(i, FIRST_HOST + i * FRAME_NS, i * FRAME_NS);
            if i == 0 {
                HelperEvent::Ready(sample)
            } else {
                HelperEvent::Frame(sample)
            }
        })
        .collect()
}

fn sealed_complete(scratch: &Scratch, name: &str, frames: u64) -> PathBuf {
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(frames));
    let out = scratch.bundle(name);
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).unwrap();
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Complete);
    out
}

fn target(target_id: &str) -> Target {
    Target {
        target_id: Some(target_id.into()),
        identity: None,
        role: "button".into(),
        label: None,
        rect_pt: Rect {
            x: 10.0,
            y: 10.0,
            width: 40.0,
            height: 20.0,
        },
    }
}

fn postcondition(
    step: &str,
    host_ns: u64,
    description: &str,
    observed: bool,
    target_id: Option<&str>,
) -> RecordEvent {
    let mut event = blank_event(EventKind::Postcondition, host_ns);
    event.step_id = Some(step.into());
    event.postcondition = Some(Postcondition {
        observed,
        observed_ns: host_ns,
        description: description.into(),
    });
    event.target = target_id.map(target);
    event
}

/// Eleven frames at 60 fps, frame `i` at host `FIRST_HOST + i * FRAME_NS` and sample
/// PTS `i * FRAME_NS`, with postconditions keyed by description and by target.
fn bundle_with_targets(scratch: &Scratch) -> PathBuf {
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(11));
    let out = scratch.bundle("targets");
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).unwrap();
    rec.pump().unwrap();
    // step-1: a description-only postcondition, then two observations of one target
    // recorded out of order. The earliest decides, so frame 6, not frame 8.
    rec.append_event(postcondition(
        "step-1",
        FIRST_HOST + 3 * FRAME_NS + 1_000,
        "dialog-closed",
        true,
        None,
    ))
    .unwrap();
    rec.append_event(postcondition(
        "step-1",
        FIRST_HOST + 8 * FRAME_NS,
        "export-sheet",
        true,
        Some("export-button"),
    ))
    .unwrap();
    rec.append_event(postcondition(
        "step-1",
        FIRST_HOST + 6 * FRAME_NS,
        "export-sheet",
        true,
        Some("export-button"),
    ))
    .unwrap();
    // step-2: the earlier observation of save-button was not observed, so the
    // later observation, on frame 9, is the one that resolves.
    rec.append_event(postcondition(
        "step-2",
        FIRST_HOST + 2 * FRAME_NS,
        "pending",
        false,
        Some("save-button"),
    ))
    .unwrap();
    rec.append_event(postcondition(
        "step-2",
        FIRST_HOST + 9 * FRAME_NS,
        "saved",
        true,
        Some("save-button"),
    ))
    .unwrap();
    // step-3 carries a description but no target, as every bundle before target ids.
    rec.append_event(postcondition(
        "step-3",
        FIRST_HOST + 9 * FRAME_NS,
        "exact",
        true,
        None,
    ))
    .unwrap();
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Complete);
    out
}

fn rewrite_json(dir: &Path, name: &str, edit: impl FnOnce(&mut Value)) {
    make_writable(dir);
    let path = dir.join(name);
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

fn append_text(dir: &Path, name: &str, text: &str) {
    make_writable(dir);
    let mut file = OpenOptions::new()
        .append(true)
        .open(dir.join(name))
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

// open_complete

#[test]
fn open_complete_accepts_a_complete_verified_bundle() {
    let scratch = Scratch::new("oc-complete");
    let out = sealed_complete(&scratch, "take", 4);
    let complete = BundleReader::open_complete(&out, &VerifyOptions::default()).unwrap();
    let plain = BundleReader::open(&out).unwrap();
    assert_eq!(complete.status(), Some(BundleStatus::Complete));
    assert_eq!(complete.frames(), plain.frames());
    assert_eq!(complete.step_index(), plain.step_index());
}

#[test]
fn open_complete_refuses_a_quarantined_bundle_and_names_the_reason() {
    let scratch = Scratch::new("oc-quarantined");
    let out = scratch.bundle("take");
    let mut backend = FakeBackend::macos();
    let mut batch = frames_batch(3);
    batch.push(HelperEvent::Exited);
    backend.push(batch);
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).unwrap();
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);

    let err = BundleReader::open_complete(&out, &VerifyOptions::default()).unwrap_err();
    assert_eq!(err.code, Code::NotComplete, "{err}");
    assert!(err.message.contains("partial_quarantined"), "{err}");
    assert!(err.message.contains("helper_lost"), "{err}");
    // open() keeps today's behaviour and still reads the quarantined bundle.
    let plain = BundleReader::open(&out).unwrap();
    assert_eq!(plain.status(), Some(BundleStatus::PartialQuarantined));
}

#[test]
fn open_complete_refuses_an_invalid_bundle_and_names_the_reason() {
    let scratch = Scratch::new("oc-invalid");
    let out = scratch.bundle("take");
    let mut rec = recorder(FakeBackend::macos(), T0);
    rec.record_start(request(out.clone())).unwrap();
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Invalid);

    let err = BundleReader::open_complete(&out, &VerifyOptions::default()).unwrap_err();
    assert_eq!(err.code, Code::NotComplete, "{err}");
    assert!(err.message.contains("invalid"), "{err}");
    assert!(err.message.contains("no_accepted_frames"), "{err}");
}

#[test]
fn open_complete_refuses_a_tampered_bundle_and_names_the_failed_check() {
    let scratch = Scratch::new("oc-tampered");
    let out = sealed_complete(&scratch, "take", 3);
    append_text(&out, "events.ndjson", "\n");
    let err = BundleReader::open_complete(&out, &VerifyOptions::default()).unwrap_err();
    assert_eq!(err.code, Code::VerifyFailed, "{err}");
    assert!(err.message.contains("events.ndjson"), "{err}");
    assert!(err.message.contains("hash does not match"), "{err}");
}

// scenario

#[test]
fn scenario_returns_each_step_with_its_label_and_say() {
    let scratch = Scratch::new("scenario");
    let out = sealed_complete(&scratch, "take", 2);
    let spec = BundleReader::open(&out).unwrap().scenario().unwrap();
    assert_eq!(
        spec,
        ScenarioSpec {
            scenario_id: "demo.one".into(),
            steps: vec![
                ScenarioStep {
                    step_id: "step-1".into(),
                    label: "Open".into(),
                    say: None,
                },
                ScenarioStep {
                    step_id: "step-2".into(),
                    label: "Save".into(),
                    say: Some("save the project".into()),
                },
            ],
        }
    );
}

#[test]
fn a_scenario_file_with_a_misplaced_step_is_not_a_bundle() {
    let scratch = Scratch::new("scenario-bad");
    let out = sealed_complete(&scratch, "take", 2);
    rewrite_json(&out, "scenario.json", |doc| {
        doc["steps"][1]["index"] = json!(5);
    });
    let err = BundleReader::open(&out).unwrap().scenario().unwrap_err();
    assert_eq!(err.code, Code::NotBundle, "{err}");
}

// events

#[test]
fn events_stream_every_line_in_file_order() {
    let scratch = Scratch::new("events");
    let out = bundle_with_targets(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    let events: Vec<RecordEvent> = reader.events().collect::<Result<_, _>>().unwrap();
    let declared = read_manifest(&out)["events"]["count"].as_u64().unwrap();
    assert_eq!(events.len() as u64, declared);
    for (position, event) in events.iter().enumerate() {
        assert_eq!(event.seq, position as u64, "seq is contiguous");
    }
    assert!(events.iter().any(|e| e.kind == EventKind::FrameReady));
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == EventKind::Postcondition)
            .count(),
        6
    );
}

#[test]
fn events_end_at_the_first_torn_line() {
    let scratch = Scratch::new("events-torn");
    let out = bundle_with_targets(&scratch);
    let declared = read_manifest(&out)["events"]["count"].as_u64().unwrap() as usize;
    append_text(&out, "events.ndjson", "{\"contract\":\"record-event\",\n");
    let reader = BundleReader::open(&out).unwrap();
    let items: Vec<_> = reader.events().collect();
    assert_eq!(items.len(), declared + 1, "the torn line is the last item");
    assert!(items[..declared].iter().all(Result::is_ok));
    let err = items[declared].as_ref().unwrap_err();
    assert_eq!(err.code, Code::NotBundle, "{err}");
    assert!(err.message.contains("line"), "{err}");
}

// clock map

#[test]
fn clock_map_maps_host_time_to_sample_and_encoded_pts() {
    let scratch = Scratch::new("clock");
    let out = sealed_complete(&scratch, "take", 4);
    let map = BundleReader::open(&out).unwrap().clock_map();
    assert_eq!(map.host_to_sample_pts_offset_ns, -(FIRST_HOST as i64));
    assert_eq!(
        map.sample_to_encoded_pts_offset_ns, 0,
        "encoded offset is 0 today"
    );
    assert_eq!(map.host_to_sample_pts(FIRST_HOST), 0);
    assert_eq!(
        map.host_to_sample_pts(FIRST_HOST + 3 * FRAME_NS),
        3 * FRAME_NS
    );
    assert_eq!(
        map.host_to_sample_pts(0),
        0,
        "before the first frame clamps to zero"
    );
    assert_eq!(map.sample_to_encoded_pts(3 * FRAME_NS), 3 * FRAME_NS);
}

#[test]
fn clock_map_honours_a_recorded_encoded_offset() {
    let scratch = Scratch::new("clock-encoded");
    let out = sealed_complete(&scratch, "take", 2);
    rewrite_json(&out, "clock.json", |doc| {
        doc["mapping"]["samplePtsToEncodedPtsOffsetNs"] = json!(1_000);
    });
    let map = BundleReader::open(&out).unwrap().clock_map();
    assert_eq!(map.sample_to_encoded_pts(5), 1_005);
}

#[test]
fn a_clock_map_without_an_encoded_offset_reads_as_zero() {
    let scratch = Scratch::new("clock-legacy");
    let out = sealed_complete(&scratch, "take", 2);
    rewrite_json(&out, "clock.json", |doc| {
        doc["mapping"]
            .as_object_mut()
            .unwrap()
            .remove("samplePtsToEncodedPtsOffsetNs");
    });
    let map = BundleReader::open(&out).unwrap().clock_map();
    assert_eq!(map.sample_to_encoded_pts_offset_ns, 0);
    assert_eq!(map.sample_to_encoded_pts(7), 7);
}

// frame_at_target

#[test]
fn frame_at_target_resolves_by_target_id_to_the_earliest_observation() {
    let scratch = Scratch::new("target-earliest");
    let out = bundle_with_targets(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(
        reader
            .frame_at_target("step-1", "export-button")
            .unwrap()
            .index,
        6
    );
}

#[test]
fn frame_at_target_ignores_unobserved_postconditions() {
    let scratch = Scratch::new("target-unobserved");
    let out = bundle_with_targets(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(
        reader
            .frame_at_target("step-2", "save-button")
            .unwrap()
            .index,
        9
    );
}

#[test]
fn frame_at_target_matches_frame_at_for_the_same_observation() {
    let scratch = Scratch::new("target-agrees");
    let out = bundle_with_targets(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    let by_target = reader
        .frame_at_target("step-2", "save-button")
        .unwrap()
        .clone();
    let by_description = reader.frame_at("step-2", "saved").unwrap();
    assert_eq!(&by_target, by_description);
}

#[test]
fn frame_at_target_is_not_found_for_unknown_targets_and_description_only_postconditions() {
    let scratch = Scratch::new("target-missing");
    let out = bundle_with_targets(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    for (step, target) in [
        ("step-1", "dialog-closed"),
        ("step-1", "no-such-target"),
        ("step-9", "export-button"),
        ("step-2", "pending"),
        ("step-3", "exact"),
    ] {
        let err = reader.frame_at_target(step, target).unwrap_err();
        assert_eq!(err.code, Code::NotFound, "{step}/{target}: {err}");
    }
    // The description-keyed path still resolves the bundle without target ids.
    assert_eq!(reader.frame_at("step-3", "exact").unwrap().index, 9);
}

// cursor assets

#[test]
fn a_bundle_without_a_cursor_manifest_has_no_cursor_assets() {
    let scratch = Scratch::new("cursor-none");
    let out = sealed_complete(&scratch, "take", 2);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(
        reader.cursor_assets_status().unwrap(),
        CursorAssetsStatus::NotRecorded
    );
    assert!(reader.cursor_assets().unwrap().is_empty());
}

fn write_cursor_manifest(dir: &Path, doc: &Value) {
    make_writable(dir);
    fs::create_dir_all(dir.join("cursors")).unwrap();
    fs::write(
        dir.join("cursors/manifest.json"),
        serde_json::to_string_pretty(doc).unwrap(),
    )
    .unwrap();
}

fn cursor_doc(path: &str, hotspot_x: u64, width: u64) -> Value {
    json!({
        "schema": "rightkit.record.cursor-assets.v1",
        "cursors": [{
            "id": "arrow",
            "path": path,
            "hotspotX": hotspot_x,
            "hotspotY": 2,
            "width": width,
            "height": 32
        }]
    })
}

#[test]
fn cursor_assets_read_the_manifest_shape() {
    let scratch = Scratch::new("cursor-read");
    let out = sealed_complete(&scratch, "take", 2);
    write_cursor_manifest(
        &out,
        &json!({
            "schema": "rightkit.record.cursor-assets.v1",
            "cursors": [
                { "id": "arrow", "path": "cursors/arrow.png", "hotspotX": 4, "hotspotY": 2, "width": 32, "height": 32 },
                { "id": "ibeam", "path": "cursors/ibeam.png", "hotspotX": 8, "hotspotY": 12, "width": 16, "height": 24 }
            ]
        }),
    );
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(
        reader.cursor_assets_status().unwrap(),
        CursorAssetsStatus::Recorded
    );
    let assets = reader.cursor_assets().unwrap();
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[1].id, "ibeam");
    assert_eq!(assets[1].path, PathBuf::from("cursors/ibeam.png"));
    assert_eq!((assets[1].hotspot_x, assets[1].hotspot_y), (8, 12));
    assert_eq!((assets[1].width, assets[1].height), (16, 24));
}

#[test]
fn cursor_assets_refuse_paths_outside_the_bundle_and_hotspots_outside_the_image() {
    let scratch = Scratch::new("cursor-bad");
    let out = sealed_complete(&scratch, "take", 2);
    for doc in [
        cursor_doc("cursors/../secret.png", 4, 32),
        cursor_doc("/etc/arrow.png", 4, 32),
        cursor_doc("arrow.png", 4, 32),
        cursor_doc("cursors/arrow.png", 40, 32),
    ] {
        write_cursor_manifest(&out, &doc);
        let err = BundleReader::open(&out)
            .unwrap()
            .cursor_assets()
            .unwrap_err();
        assert_eq!(err.code, Code::NotBundle, "{doc}: {err}");
    }
}

#[test]
fn cursor_assets_refuse_an_unknown_schema() {
    let scratch = Scratch::new("cursor-schema");
    let out = sealed_complete(&scratch, "take", 2);
    let mut doc = cursor_doc("cursors/arrow.png", 4, 32);
    doc["schema"] = json!("rightkit.record.cursor-assets.v9");
    write_cursor_manifest(&out, &doc);
    let err = BundleReader::open(&out)
        .unwrap()
        .cursor_assets()
        .unwrap_err();
    assert_eq!(err.code, Code::NotBundle, "{err}");
}

#[test]
fn a_missing_bundle_directory_is_not_found_for_open_complete() {
    let scratch = Scratch::new("oc-missing");
    let err = BundleReader::open_complete(&scratch.bundle("absent"), &VerifyOptions::default())
        .unwrap_err();
    assert_eq!(err.code, Code::NotFound, "{err}");
}
