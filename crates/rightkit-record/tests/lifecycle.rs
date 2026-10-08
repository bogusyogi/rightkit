mod common;

use common::{blank_event, read_manifest, recorder, request, Scratch};
use rightkit_contracts::types::record_event::DropReason;
use rightkit_contracts::types::record_session::{BundleStatus, VisibilityState};
use rightkit_record::fake::{frame, FakeBackend};
use rightkit_record::{
    Code, Codec, Failure, HelperEvent, Permission, Phase, PixelMasking, VerifyOptions,
};

const T0: u64 = 1_000_000;

fn ready_and_frames(n: u64) -> Vec<HelperEvent> {
    (0..n)
        .map(|i| {
            let sample = frame(i, 5_000 + i * 16_666_667, i * 16_666_667);
            if i == 0 {
                HelperEvent::Ready(sample)
            } else {
                HelperEvent::Frame(sample)
            }
        })
        .collect()
}

fn start_take(backend: FakeBackend, scratch: &Scratch) -> (common::Rec, std::path::PathBuf) {
    let out = scratch.bundle("take");
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).expect("start");
    (rec, out)
}

#[test]
fn clean_stop_seals_a_complete_bundle() {
    let scratch = Scratch::new("complete");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(3));
    let (mut rec, out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().expect("stop");
    assert_eq!(outcome.status, BundleStatus::Complete, "{outcome:?}");
    assert_eq!(outcome.reason, None);
    assert_eq!(outcome.manifest_problem, None);
    let report = rec.bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    assert_eq!(read_manifest(&out)["status"], "complete");
    assert_eq!(read_manifest(&out)["frames"]["count"], 3);
}

#[test]
fn stop_is_idempotent_and_returns_the_same_outcome() {
    let scratch = Scratch::new("idempotent");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(2));
    let (mut rec, _out) = start_take(backend, &scratch);
    let first = rec.record_stop().unwrap();
    let second = rec.record_stop().unwrap();
    assert_eq!(first, second);
    assert_eq!(rec.backend().stops, 1, "the helper is stopped once");
}

#[test]
fn stop_without_a_take_is_refused() {
    let mut rec = recorder(FakeBackend::macos(), T0);
    assert_eq!(rec.record_stop().unwrap_err().code, Code::NotRecording);
}

#[test]
fn a_second_start_while_recording_is_refused() {
    let scratch = Scratch::new("twice");
    let (mut rec, _out) = start_take(FakeBackend::macos(), &scratch);
    let err = rec
        .record_start(request(scratch.bundle("other")))
        .unwrap_err();
    assert_eq!(err.code, Code::AlreadyRecording);
}

#[test]
fn stop_with_no_accepted_frame_is_invalid_not_complete() {
    let scratch = Scratch::new("no-frames");
    let (mut rec, out) = start_take(FakeBackend::macos(), &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid);
    assert_eq!(outcome.reason.as_deref(), Some("no_accepted_frames"));
    assert!(!rec
        .bundle_verify(&out, &VerifyOptions::default())
        .unwrap()
        .ok());
}

#[test]
fn helper_death_quarantines_and_is_never_complete() {
    let scratch = Scratch::new("helper-death");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(2);
    batch.push(HelperEvent::Exited);
    backend.push(batch);
    let (mut rec, out) = start_take(backend, &scratch);
    rec.pump().unwrap();
    let status = rec.record_status().unwrap();
    assert_eq!(status.phase, Phase::Quarantined);
    assert_eq!(status.failure.as_deref(), Some("helper_lost"));
    assert_eq!(rec.backend().aborts, 1);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);
    assert_eq!(outcome.reason.as_deref(), Some("helper_lost"));
    assert!(rec
        .bundle_verify(&out, &VerifyOptions::default())
        .unwrap()
        .ok());
}

#[test]
fn a_failed_stop_call_is_quarantined() {
    let scratch = Scratch::new("stop-error");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(2));
    backend.stop_error = Some(rightkit_record::RecordError::new(
        Code::Helper,
        "helper gone",
    ));
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);
    assert_eq!(outcome.reason.as_deref(), Some("helper_lost"));
}

#[test]
fn an_unclean_finish_is_quarantined() {
    let scratch = Scratch::new("unclean");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(2));
    backend.clean_finish = false;
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);
    assert!(outcome.reason.unwrap().starts_with("io_error"));
}

#[test]
fn disk_full_quarantines_mid_take() {
    let scratch = Scratch::new("disk-full");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(2);
    batch.push(HelperEvent::Failed(Failure::DiskFull));
    backend.push(batch);
    let (mut rec, _out) = start_take(backend, &scratch);
    let status = rec.record_status().unwrap();
    assert_eq!(status.phase, Phase::Quarantined);
    assert_eq!(status.failure.as_deref(), Some("disk_full"));
    assert_ne!(rec.record_stop().unwrap().status, BundleStatus::Complete);
}

#[test]
fn target_exit_quarantines_mid_take() {
    let scratch = Scratch::new("target-exit");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(2);
    batch.push(HelperEvent::Failed(Failure::TargetExited));
    backend.push(batch);
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);
    assert_eq!(outcome.reason.as_deref(), Some("target_exited"));
}

#[test]
fn permission_revoked_mid_take_is_invalid() {
    let scratch = Scratch::new("revoked");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(2);
    batch.push(HelperEvent::Failed(Failure::PermissionRevoked));
    backend.push(batch);
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid);
    assert_eq!(outcome.reason.as_deref(), Some("permission_revoked"));
}

#[test]
fn mask_geometry_lost_mid_take_quarantines() {
    let scratch = Scratch::new("mask-lost");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(2);
    batch.push(HelperEvent::Failed(Failure::MaskGeometryUnknown));
    backend.push(batch);
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);
    assert_eq!(outcome.reason.as_deref(), Some("mask_geometry_unknown"));
}

#[test]
fn events_after_a_failure_are_ignored() {
    let scratch = Scratch::new("after-failure");
    let mut backend = FakeBackend::macos();
    let mut first = ready_and_frames(2);
    first.push(HelperEvent::Failed(Failure::DiskFull));
    backend.push(first);
    backend.push(vec![HelperEvent::Frame(frame(9, 99, 99))]);
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.reason.as_deref(), Some("disk_full"));
    assert_eq!(rec.record_status().unwrap().frames, 2);
}

#[test]
fn frame_before_the_readiness_marker_is_a_protocol_violation() {
    let scratch = Scratch::new("no-marker");
    let mut backend = FakeBackend::macos();
    backend.push(vec![HelperEvent::Frame(frame(0, 10, 0))]);
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid);
    assert!(outcome
        .reason
        .unwrap()
        .contains("before the readiness marker"));
}

#[test]
fn readiness_marker_sent_twice_is_invalid() {
    let scratch = Scratch::new("double-marker");
    let mut backend = FakeBackend::macos();
    backend.push(vec![
        HelperEvent::Ready(frame(0, 10, 0)),
        HelperEvent::Ready(frame(1, 20, 1)),
    ]);
    let (mut rec, _out) = start_take(backend, &scratch);
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Invalid);
}

#[test]
fn pts_that_goes_backwards_is_invalid() {
    let scratch = Scratch::new("pts");
    let mut backend = FakeBackend::macos();
    backend.push(vec![
        HelperEvent::Ready(frame(0, 10, 100)),
        HelperEvent::Frame(frame(1, 20, 50)),
    ]);
    let (mut rec, _out) = start_take(backend, &scratch);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid);
    assert!(outcome.reason.unwrap().contains("PTS"));
}

#[test]
fn a_malformed_raw_hash_is_invalid() {
    let scratch = Scratch::new("hash");
    let mut backend = FakeBackend::macos();
    let mut bad = frame(0, 10, 0);
    bad.raw_hash = "md5:abc".into();
    backend.push(vec![HelperEvent::Ready(bad)]);
    let (mut rec, _out) = start_take(backend, &scratch);
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Invalid);
}

#[test]
fn caller_may_not_write_frame_events() {
    let scratch = Scratch::new("caller-frame");
    let (mut rec, _out) = start_take(FakeBackend::macos(), &scratch);
    let err = rec
        .append_event(blank_event(
            rightkit_contracts::types::record_event::EventKind::FrameReady,
            1,
        ))
        .unwrap_err();
    assert_eq!(err.code, Code::ArgInvalid);
}

#[test]
fn permission_denied_at_start_writes_nothing() {
    let scratch = Scratch::new("denied");
    let mut backend = FakeBackend::macos();
    backend.set_permission(Permission::Denied);
    let mut rec = recorder(backend, T0);
    let out = scratch.bundle("denied");
    let err = rec.record_start(request(out.clone())).unwrap_err();
    assert_eq!(err.code, Code::PermissionDenied);
    let entries: Vec<_> = std::fs::read_dir(&out).unwrap().collect();
    assert!(entries.is_empty(), "the bundle directory is empty");
    assert_eq!(rec.backend().starts, 0);
}

#[test]
fn unsupported_visibility_is_refused_before_any_write() {
    let scratch = Scratch::new("unsupported");
    let mut rec = recorder(FakeBackend::macos(), T0);
    let out = scratch.bundle("hidden");
    let mut req = request(out.clone());
    req.visibility = VisibilityState::AppHidden;
    assert_eq!(rec.record_start(req).unwrap_err().code, Code::Unsupported);
    assert!(!out.exists());
}

#[test]
fn unknown_mask_geometry_is_refused_before_capture() {
    let scratch = Scratch::new("mask");
    let mut rec = recorder(FakeBackend::macos(), T0);
    let out = scratch.bundle("mask");
    let mut req = request(out.clone());
    req.masking = PixelMasking::Unknown;
    assert_eq!(
        rec.record_start(req).unwrap_err().code,
        Code::MaskGeometryUnknown
    );
    assert!(!out.exists());
    assert_eq!(rec.backend().starts, 0);
}

#[test]
fn a_backend_start_failure_leaves_an_invalid_bundle() {
    let scratch = Scratch::new("start-fail");
    let mut backend = FakeBackend::macos();
    backend.start_error = Some(rightkit_record::RecordError::new(Code::Helper, "no helper"));
    let mut rec = recorder(backend, T0);
    let out = scratch.bundle("start-fail");
    assert!(rec.record_start(request(out.clone())).is_err());
    assert_eq!(read_manifest(&out)["status"], "invalid");
}

#[test]
fn a_stale_take_is_sealed_on_open_and_a_fresh_one_is_not() {
    let scratch = Scratch::new("stale");
    let root = scratch.path().to_path_buf();
    let stale_out = scratch.bundle("stale");
    let fresh_out = scratch.bundle("fresh");

    // A take that started at T0 and then its process died: no stop, no further heartbeat.
    {
        let mut backend = FakeBackend::macos();
        backend.push(ready_and_frames(3));
        let mut rec = recorder(backend, T0);
        rec.record_start(request(stale_out.clone())).unwrap();
        rec.pump().unwrap();
    }
    // A take still alive, last heartbeat at T0 + 50 s.
    let mut live = recorder(FakeBackend::macos(), T0 + 50_000);
    live.record_start(request(fresh_out.clone())).unwrap();
    live.pump().unwrap();

    // Sweep at T0 + 60 s: the stale take is 60 s old (window 30 s), the live one 10 s.
    let (_rec, recovered) = rightkit_record::Recorder::open(
        FakeBackend::macos(),
        rightkit_record::ManualClock::new(T0 + 60_000),
        std::slice::from_ref(&root),
        30_000,
    )
    .unwrap();
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    let stale = &recovered[0];
    assert_eq!(stale.dir, stale_out);
    assert_eq!(stale.status, BundleStatus::PartialQuarantined);
    assert_eq!(stale.reason.as_deref(), Some("stale_session_recovered"));
    let report =
        rightkit_record::verify::bundle_verify(&stale_out, &VerifyOptions::default()).unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    assert_eq!(report.status, Some(BundleStatus::PartialQuarantined));

    // The live take is untouched and still recording.
    assert!(!fresh_out.join("hashes.json").exists());
    assert_eq!(live.record_status().unwrap().phase, Phase::Recording);

    // Sealed takes are never swept again.
    let (_rec2, again) = rightkit_record::Recorder::open(
        FakeBackend::macos(),
        rightkit_record::ManualClock::new(T0 + 60_000),
        std::slice::from_ref(&root),
        30_000,
    )
    .unwrap();
    assert!(again.is_empty(), "{again:?}");
}

#[test]
fn a_stale_take_with_no_ready_frame_is_invalid() {
    let scratch = Scratch::new("stale-no-ready");
    let out = scratch.bundle("take");
    {
        let mut rec = recorder(FakeBackend::macos(), T0);
        rec.record_start(request(out.clone())).unwrap();
    }
    let (_rec, recovered) = rightkit_record::Recorder::open(
        FakeBackend::macos(),
        rightkit_record::ManualClock::new(T0 + 60_000),
        &[scratch.path().to_path_buf()],
        30_000,
    )
    .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].status, BundleStatus::Invalid);
    assert_eq!(read_manifest(&out)["status"], "invalid");
}

#[test]
fn status_reports_frames_and_drops() {
    let scratch = Scratch::new("status");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(4);
    batch.push(HelperEvent::Dropped {
        host_ns: 9,
        count: 2,
        interval_ms: 33.3,
        reason: DropReason::Backpressure,
    });
    backend.push(batch);
    let (mut rec, _out) = start_take(backend, &scratch);
    let status = rec.record_status().unwrap();
    assert_eq!(status.phase, Phase::Recording);
    assert_eq!(status.frames, 4);
    assert_eq!(status.dropped, 2);
}

#[test]
fn undetermined_permission_is_decided_by_the_start_prompt() {
    let scratch = Scratch::new("undetermined-granted");
    let mut backend = FakeBackend::macos();
    backend.set_permission(Permission::Undetermined);
    let mut rec = recorder(backend, T0);
    assert_eq!(rec.doctor().permission, Permission::Undetermined);
    let out = scratch.bundle("granted");
    rec.record_start(request(out.clone()))
        .expect("the prompt grants");
    assert_eq!(rec.doctor().permission, Permission::Granted);
    rec.backend_mut().push(ready_and_frames(2));
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Complete);
}

#[test]
fn undetermined_permission_declined_at_the_prompt_is_refused() {
    let scratch = Scratch::new("undetermined-declined");
    let mut backend = FakeBackend::macos();
    backend.set_permission(Permission::Undetermined);
    backend.prompt_grants = false;
    let mut rec = recorder(backend, T0);
    let out = scratch.bundle("declined");
    let err = rec.record_start(request(out.clone())).unwrap_err();
    assert_eq!(err.code, Code::PermissionDenied);
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid);
    assert_eq!(
        read_capture(&out),
        serde_json::json!({}),
        "no capture was started"
    );
}

#[test]
fn capture_records_the_codec_and_the_applied_scale() {
    let scratch = Scratch::new("codec-scale");
    let mut backend = FakeBackend::macos();
    backend.applied_scale = Some(1.0);
    let mut rec = recorder(backend, T0);
    let out = scratch.bundle("take");
    let mut req = request(out.clone());
    req.codec = Codec::Hevc;
    req.scale = 0.5;
    rec.record_start(req).expect("start");
    assert_eq!(rec.backend().last_plan.as_ref().unwrap().codec, Codec::Hevc);
    rec.backend_mut().push(ready_and_frames(2));
    rec.record_stop().unwrap();
    let capture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["codec"], "hevc");
    assert_eq!(
        capture["scale"], 1.0,
        "the applied scale, not the requested 0.5"
    );
}

#[test]
fn a_visibility_mismatch_at_start_is_refused_and_sealed_invalid() {
    let scratch = Scratch::new("visibility-mismatch");
    let mut backend = FakeBackend::macos();
    backend.observed_visibility = Some(VisibilityState::OnScreenVisible);
    let mut rec = recorder(backend, T0);
    let out = scratch.bundle("take");
    let err = rec.record_start(request(out.clone())).unwrap_err();
    assert_eq!(err.code, Code::Unsupported);
    assert!(err.message.contains("visibility_mismatch"), "{err}");
    assert_eq!(
        read_capture(&out),
        serde_json::json!({}),
        "no capture metadata is written"
    );
    assert_eq!(read_manifest(&out)["status"], "invalid");
}

#[test]
fn a_matching_observed_visibility_starts_and_is_recorded() {
    let scratch = Scratch::new("visibility-match");
    let mut backend = FakeBackend::macos();
    backend.observed_visibility = Some(VisibilityState::OffScreenVisible);
    let (mut rec, out) = start_take(backend, &scratch);
    rec.backend_mut().push(ready_and_frames(2));
    rec.record_stop().unwrap();
    let capture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["visibilityState"], "off_screen_visible");
}

#[test]
fn window_hidden_or_minimized_mid_take_quarantines_with_its_own_reason() {
    for (failure, reason) in [
        (Failure::WindowHidden, "window_hidden"),
        (Failure::WindowMinimized, "window_minimized"),
    ] {
        let scratch = Scratch::new(reason);
        let mut backend = FakeBackend::windows();
        let mut batch = ready_and_frames(2);
        batch.push(HelperEvent::Failed(failure));
        backend.push(batch);
        let (mut rec, _out) = start_take(backend, &scratch);
        let outcome = rec.record_stop().unwrap();
        assert_eq!(outcome.status, BundleStatus::PartialQuarantined, "{reason}");
        assert_eq!(outcome.reason.as_deref(), Some(reason));
    }
}

/// The sealed `capture.json` of a take. Refused takes seal it as `{}`.
fn read_capture(out: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(out.join("capture.json")).unwrap()).unwrap()
}

#[test]
fn helper_death_records_what_the_media_still_plays() {
    let scratch = Scratch::new("recovered-media");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(3);
    batch.push(HelperEvent::Exited);
    backend.push(batch);
    let (mut rec, out) = start_take(backend, &scratch);
    rec.pump().unwrap();
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined);
    // The reason is the failure; the recovered figures ride in the manifest and the outcome.
    assert_eq!(outcome.reason.as_deref(), Some("helper_lost"));
    let media = outcome.recovered_media.expect("playable media is recorded");
    assert_eq!(media.samples, 30);
    assert_eq!(media.accepted_frames, 3);
    assert_eq!(media.duration_ms, 30 * 33);
    let manifest = read_manifest(&out);
    let reason = manifest["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("helper_lost; recovered_media frames=30/3 duration_ms=990"),
        "{reason}"
    );
    // The bundle reader sees the same accepted frames the media note compares against.
    let reader = rightkit_record::BundleReader::open(&out).unwrap();
    assert_eq!(reader.frames().len() as u64, media.accepted_frames);
    assert!(rec
        .bundle_verify(&out, &VerifyOptions::default())
        .unwrap()
        .ok());
}

#[test]
fn media_that_does_not_play_makes_the_take_explicitly_invalid() {
    let scratch = Scratch::new("unplayable-media");
    let mut backend = FakeBackend::macos();
    let mut batch = ready_and_frames(3);
    batch.push(HelperEvent::Exited);
    backend.push(batch);
    let (mut rec, out) = start_take(backend, &scratch);
    // The helper died before it wrote one whole fragment: only a header is on disk.
    let bytes = rightkit_record::media::synthetic_fragmented_mp4(30, 33, 30);
    let moof_at = bytes.windows(4).position(|w| w == b"moof").unwrap() - 4;
    std::fs::write(out.join("clean.mp4"), &bytes[..moof_at]).unwrap();
    rec.pump().unwrap();
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid, "{outcome:?}");
    assert!(outcome
        .reason
        .as_deref()
        .unwrap()
        .contains("media_unplayable"));
    assert_eq!(outcome.recovered_media, None);
}

#[test]
fn a_stale_take_is_recovered_with_its_media_duration() {
    let scratch = Scratch::new("stale-media");
    let out = scratch.bundle("stale");
    {
        let mut backend = FakeBackend::macos();
        backend.push(ready_and_frames(3));
        let mut rec = recorder(backend, T0);
        rec.record_start(request(out.clone())).unwrap();
        rec.pump().unwrap();
        // The process dies here: no stop, and the media is cut inside its last fragment.
        let bytes = rightkit_record::media::synthetic_fragmented_mp4(60, 33, 30);
        std::fs::write(out.join("clean.mp4"), &bytes[..bytes.len() - 10]).unwrap();
    }
    let (_rec, recovered) = rightkit_record::Recorder::open(
        FakeBackend::macos(),
        rightkit_record::ManualClock::new(T0 + 60_000),
        std::slice::from_ref(&scratch.path().to_path_buf()),
        30_000,
    )
    .unwrap();
    assert_eq!(recovered.len(), 1);
    let media = recovered[0].media.expect("media recovered");
    assert_eq!(media.samples, 30, "the torn second fragment is not counted");
    assert_eq!(media.duration_ms, 990);
}
