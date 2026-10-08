mod common;

use std::fs;

use common::{make_writable, read_manifest, recorder, request, Scratch};
use rightkit_contracts::types::record_session::{BundleStatus, TrackKind};
use rightkit_record::fake::{frame, FakeBackend};
use rightkit_record::{HelperEvent, VerifyOptions};
use serde_json::{json, Value};

const T0: u64 = 1_000_000;

fn sealed(scratch: &Scratch, backend: FakeBackend, name: &str) -> std::path::PathBuf {
    let out = scratch.bundle(name);
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).unwrap();
    let outcome = rec.record_stop().unwrap();
    assert_eq!(outcome.status, BundleStatus::Complete, "{outcome:?}");
    out
}

fn frames_batch(n: u64) -> Vec<HelperEvent> {
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

fn verify(out: &std::path::Path) -> rightkit_record::VerifyReport {
    rightkit_record::verify::bundle_verify(out, &VerifyOptions::default()).unwrap()
}

#[test]
fn sealed_bundle_holds_exactly_the_contract_files() {
    let scratch = Scratch::new("files");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(3));
    let out = sealed(&scratch, backend, "take");
    let mut names: Vec<String> = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "capture.json",
            "clean.mp4",
            "clock.json",
            "events.ndjson",
            "hashes.json",
            "scenario.json"
        ]
    );
    assert!(verify(&out).ok());
}

#[test]
fn sealed_bundle_is_read_only_and_refuses_writes() {
    let scratch = Scratch::new("readonly");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = sealed(&scratch, backend, "take");
    let events = out.join("events.ndjson");
    assert!(fs::metadata(&out).unwrap().permissions().readonly());
    // File-level refusal, on every OS: each sealed file carries the read-only
    // attribute and refuses an append and a rewrite.
    let mut files = Vec::new();
    collect_files(&out, &mut files);
    assert!(files.contains(&events), "the event log is a sealed file");
    for file in &files {
        assert!(
            fs::metadata(file).unwrap().permissions().readonly(),
            "{} must carry the read-only attribute",
            file.display()
        );
        assert!(
            fs::OpenOptions::new().append(true).open(file).is_err(),
            "appending to {} must fail",
            file.display()
        );
        assert!(
            fs::write(file, b"x").is_err(),
            "rewriting {} must fail",
            file.display()
        );
    }
    // Unix-only: a Windows directory read-only flag does not stop new files.
    #[cfg(unix)]
    assert!(fs::write(out.join("stray.txt"), b"x").is_err());
}

fn collect_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn tampering_with_an_event_fails_the_hash_check() {
    let scratch = Scratch::new("tamper");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = sealed(&scratch, backend, "take");
    make_writable(&out);
    let mut text = fs::read_to_string(out.join("events.ndjson")).unwrap();
    text.push_str("{}\n");
    fs::write(out.join("events.ndjson"), text).unwrap();
    let report = verify(&out);
    assert!(!report.ok());
    assert!(report
        .problems
        .iter()
        .any(|p| p.starts_with("events.ndjson: hash")));
}

#[test]
fn an_unlisted_file_fails_verification() {
    let scratch = Scratch::new("stray");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = sealed(&scratch, backend, "take");
    make_writable(&out);
    fs::write(out.join("notes.txt"), b"left behind").unwrap();
    let report = verify(&out);
    assert!(report
        .problems
        .iter()
        .any(|p| p.contains("notes.txt: unlisted")));
}

#[test]
fn working_state_left_behind_fails_verification() {
    let scratch = Scratch::new("working");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = sealed(&scratch, backend, "take");
    make_writable(&out);
    fs::write(out.join("state.json"), b"{}").unwrap();
    let report = verify(&out);
    assert!(report.problems.iter().any(|p| p.contains("state.json")));
}

#[test]
fn capture_json_must_match_the_manifest() {
    let scratch = Scratch::new("capture");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = sealed(&scratch, backend, "take");
    make_writable(&out);
    // Rewrite the file and its manifest entry, so only the capture comparison can catch it.
    let mut capture: Value =
        serde_json::from_str(&fs::read_to_string(out.join("capture.json")).unwrap()).unwrap();
    capture["fps"] = json!(30);
    fs::write(
        out.join("capture.json"),
        serde_json::to_string_pretty(&capture).unwrap(),
    )
    .unwrap();
    let mut manifest = read_manifest(&out);
    let (hash, bytes) = rightkit_record::hash::file_hash(&out.join("capture.json")).unwrap();
    manifest["files"]["capture.json"] = json!({ "blake3": hash, "bytes": bytes });
    fs::write(
        out.join("hashes.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let report = verify(&out);
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("capture.json does not match")),
        "{:?}",
        report.problems
    );
}

#[test]
fn audio_tracks_are_listed_and_hashed() {
    let scratch = Scratch::new("tracks");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = scratch.bundle("take");
    let mut req = request(out.clone());
    req.audio = vec![TrackKind::AppAudio, TrackKind::Microphone];
    let mut rec = recorder(backend, T0);
    rec.record_start(req).unwrap();
    assert_eq!(rec.record_stop().unwrap().status, BundleStatus::Complete);
    let manifest = read_manifest(&out);
    let tracks = manifest["tracks"].as_array().expect("tracks listed");
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["file"], "tracks/app_audio.m4a");
    assert!(verify(&out).ok());
}

#[test]
fn geometry_changes_open_new_epochs_and_close_the_old() {
    let scratch = Scratch::new("geometry");
    let mut backend = FakeBackend::macos();
    let mut batch = frames_batch(2);
    batch.push(HelperEvent::Geometry {
        host_ns: 1_000_000_000 + 500_000_000,
        bounds_pt: rightkit_contracts::types::Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 700.0,
        },
        scale: 2.0,
        dpi: 192,
    });
    backend.push(batch);
    let out = sealed(&scratch, backend, "take");
    let manifest = read_manifest(&out);
    let epochs = manifest["geometryEpochs"].as_array().unwrap();
    assert_eq!(epochs.len(), 2);
    assert_eq!(epochs[0]["endMs"], 500);
    assert_eq!(epochs[1]["startMs"], 500);
    assert!(verify(&out).ok());
}

#[test]
fn dropped_frames_are_counted_and_verified() {
    let scratch = Scratch::new("dropped");
    let mut backend = FakeBackend::macos();
    let mut batch = frames_batch(2);
    batch.push(HelperEvent::Dropped {
        host_ns: 9_000,
        count: 3,
        interval_ms: 50.0,
        reason: rightkit_contracts::types::record_event::DropReason::Backpressure,
    });
    backend.push(batch);
    let out = sealed(&scratch, backend, "take");
    let manifest = read_manifest(&out);
    assert_eq!(manifest["frames"]["droppedCount"], 3);
    let report = verify(&out);
    assert!(report.ok(), "{:?}", report.problems);
}

#[test]
fn complete_bundles_have_a_schema_valid_manifest_and_complete_status() {
    let scratch = Scratch::new("manifest");
    let mut backend = FakeBackend::macos();
    backend.push(frames_batch(2));
    let out = sealed(&scratch, backend, "take");
    let manifest = read_manifest(&out);
    assert_eq!(manifest["status"], "complete");
    assert!(
        manifest.get("reason").is_none(),
        "a complete bundle carries no reason"
    );
    assert_eq!(manifest["donorDirectoryRequired"], false);
    assert_eq!(manifest["redaction"]["policyId"], "redaction.rk3");
    assert!(
        rightkit_contracts::validate(rightkit_contracts::Contract::RecordSession, &manifest)
            .is_ok()
    );
}
