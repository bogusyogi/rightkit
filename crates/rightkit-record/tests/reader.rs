mod common;

use common::{blank_event, recorder, request, Scratch};
use rightkit_contracts::types::record_event::{EventKind, Postcondition};
use rightkit_contracts::types::record_session::BundleStatus;
use rightkit_record::fake::{frame, FakeBackend};
use rightkit_record::{BundleReader, Code, HelperEvent};

const T0: u64 = 1_000_000;
const FRAME_NS: u64 = 16_666_667;
const FIRST_HOST: u64 = 5_000_000;

/// Eleven frames at 60 fps. Frame `i` has host time `FIRST_HOST + i * FRAME_NS`
/// and sample PTS `i * FRAME_NS`.
fn bundle_with_frames(scratch: &Scratch) -> std::path::PathBuf {
    let mut backend = FakeBackend::macos();
    let mut batch = Vec::new();
    for i in 0..11u64 {
        let sample = frame(i, FIRST_HOST + i * FRAME_NS, i * FRAME_NS);
        batch.push(if i == 0 {
            HelperEvent::Ready(sample)
        } else {
            HelperEvent::Frame(sample)
        });
    }
    backend.push(batch);
    let out = scratch.bundle("take");
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).unwrap();
    rec.pump().unwrap();
    // step-1 runs and its result is observed at host time just after frame 3.
    rec.append_event(step_event(
        "step-1",
        EventKind::Input,
        FIRST_HOST + 3 * FRAME_NS - 100,
        None,
    ))
    .unwrap();
    rec.append_event(step_event(
        "step-1",
        EventKind::Postcondition,
        FIRST_HOST + 3 * FRAME_NS + 1_000,
        Some(("dialog-closed", true)),
    ))
    .unwrap();
    // step-2 has two observations of the same postcondition, plus one not observed.
    rec.append_event(step_event(
        "step-2",
        EventKind::Postcondition,
        FIRST_HOST + 7 * FRAME_NS,
        Some(("saved", true)),
    ))
    .unwrap();
    rec.append_event(step_event(
        "step-2",
        EventKind::Postcondition,
        FIRST_HOST + 6 * FRAME_NS,
        Some(("saved", true)),
    ))
    .unwrap();
    rec.append_event(step_event(
        "step-2",
        EventKind::Postcondition,
        FIRST_HOST + 2 * FRAME_NS,
        Some(("pending", false)),
    ))
    .unwrap();
    // step-3 is observed on the exact frame boundary of frame 9.
    rec.append_event(step_event(
        "step-3",
        EventKind::Postcondition,
        FIRST_HOST + 9 * FRAME_NS,
        Some(("exact", true)),
    ))
    .unwrap();
    // step-4 is observed after the last frame, so there is no frame to show it.
    rec.append_event(step_event(
        "step-4",
        EventKind::Postcondition,
        FIRST_HOST + 50 * FRAME_NS,
        Some(("late", true)),
    ))
    .unwrap();
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Complete);
    out
}

/// Input events carry the dispatch fields the schema requires.
fn input_fields(event: &mut rightkit_contracts::types::record_event::RecordEvent) {
    use rightkit_contracts::types::record_event::{InputKind, Phase};
    event.input_kind = Some(InputKind::Click);
    event.phase = Some(Phase::Down);
    event.effect_id = Some("effect-1".into());
    event.admitted = Some(true);
}

fn step_event(
    step: &str,
    kind: EventKind,
    host_ns: u64,
    post: Option<(&str, bool)>,
) -> rightkit_contracts::types::record_event::RecordEvent {
    let mut event = blank_event(kind, host_ns);
    event.step_id = Some(step.into());
    if kind == EventKind::Input {
        input_fields(&mut event);
    }
    if let Some((description, observed)) = post {
        event.postcondition = Some(Postcondition {
            observed,
            observed_ns: host_ns,
            description: description.into(),
        });
    }
    event
}

#[test]
fn postcondition_resolves_to_the_first_frame_after_it() {
    let scratch = Scratch::new("reader-after");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(reader.status(), Some(BundleStatus::Complete));
    let frame = reader.frame_at("step-1", "dialog-closed").unwrap();
    assert_eq!(
        frame.index, 4,
        "the frame after the postcondition at frame 3 plus 1 us"
    );
    assert_eq!(frame.sample_pts_ns, 4 * FRAME_NS);
    assert_eq!(
        frame.raw_blake3,
        rightkit_record::hash::blake3_text(&4u64.to_le_bytes())
    );
}

#[test]
fn a_postcondition_on_a_frame_boundary_returns_that_frame() {
    let scratch = Scratch::new("reader-exact");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(reader.frame_at("step-3", "exact").unwrap().index, 9);
}

#[test]
fn repeated_observations_use_the_earliest() {
    let scratch = Scratch::new("reader-earliest");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(reader.frame_at("step-2", "saved").unwrap().index, 6);
}

#[test]
fn unobserved_postconditions_are_not_resolved() {
    let scratch = Scratch::new("reader-unobserved");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    let err = reader.frame_at("step-2", "pending").unwrap_err();
    assert_eq!(err.code, Code::NotFound);
}

#[test]
fn a_postcondition_with_no_later_frame_is_not_found() {
    let scratch = Scratch::new("reader-late");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(
        reader.frame_at("step-4", "late").unwrap_err().code,
        Code::NotFound
    );
}

#[test]
fn an_unknown_step_or_postcondition_is_not_found() {
    let scratch = Scratch::new("reader-unknown");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(
        reader.frame_at("step-9", "saved").unwrap_err().code,
        Code::NotFound
    );
    assert_eq!(
        reader.frame_at("step-2", "never-said").unwrap_err().code,
        Code::NotFound
    );
}

#[test]
fn step_index_spans_each_steps_events() {
    let scratch = Scratch::new("reader-index");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    let span = reader.step_index()["step-1"];
    assert_eq!(span.first_host_ns, FIRST_HOST + 3 * FRAME_NS - 100);
    assert_eq!(span.last_host_ns, FIRST_HOST + 3 * FRAME_NS + 1_000);
    assert_eq!(
        reader.step_index()["step-2"].first_host_ns,
        FIRST_HOST + 2 * FRAME_NS
    );
    assert_eq!(
        reader.step_index()["step-2"].last_host_ns,
        FIRST_HOST + 7 * FRAME_NS
    );
}

#[test]
fn frames_are_exposed_in_pts_order() {
    let scratch = Scratch::new("reader-frames");
    let out = bundle_with_frames(&scratch);
    let reader = BundleReader::open(&out).unwrap();
    assert_eq!(reader.frames().len(), 11);
    assert!(reader
        .frames()
        .windows(2)
        .all(|w| w[0].sample_pts_ns < w[1].sample_pts_ns));
}

#[test]
fn a_bundle_without_a_clock_map_is_not_a_bundle() {
    let scratch = Scratch::new("reader-noclock");
    let out = scratch.bundle("missing");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("hashes.json"), b"{}").unwrap();
    assert_eq!(
        BundleReader::open(&out).err().unwrap().code,
        Code::NotBundle
    );
}
