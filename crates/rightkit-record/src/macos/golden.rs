//! Golden vectors for the Swift helper's side of the wire protocol.
//!
//! The Rust protocol and `serve_backend` produce the frames; the Swift tests in
//! `helpers/macos/Tests/RecordWireTests` decode the same file and must (a) parse every request
//! the core sends, (b) produce every helper frame byte-for-meaning identically, and (c) compute
//! the same BLAKE3 hashes. Regenerate after a protocol change by running the built test binary
//! with the variable set (the build broker does not forward it):
//! `RK_UPDATE_GOLDEN=1 <rightkit_record test binary> macos::golden`, then rerun the Swift tests.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use rightkit_contracts::types::record_event::DropReason;
use rightkit_contracts::types::record_session::{TrackKind, VisibilityState};
use rightkit_contracts::types::Rect;
use rightkit_sidecar::length_prefixed::{read_frame_bytes, write_frame};
use serde_json::{json, Value};

use crate::backend::{BackendPlan, Codec, Failure, FrameSample, HelperEvent, Permission, Source};
use crate::fake::FakeBackend;
use crate::hash::blake3_text;
use crate::plan::PixelMasking;
use crate::process::serve_backend;
use crate::protocol::{encode_plan, MAX_FRAME_BYTES};

const OUT_DIR: &str = "/tmp/rightkit-record-golden/take";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("helpers/macos/Tests/RecordWireTests/golden.json")
}

/// The request frame the core's link writes (`ProcessBackend::call`).
fn request(id: u64, op: &str, params: Value) -> Value {
    let mut frame = json!({ "kind": "request", "id": id, "op": op });
    if let (Value::Object(target), Value::Object(extra)) = (&mut frame, params) {
        target.extend(extra);
    }
    frame
}

/// Runs `serve_backend` over the scripted requests and returns the frames it wrote.
fn serve(requests: &[Value], backend: FakeBackend) -> Vec<Value> {
    let mut input = Vec::new();
    for frame in requests {
        write_frame(&mut input, frame, MAX_FRAME_BYTES).unwrap();
    }
    let mut backend = backend;
    let mut output = Vec::new();
    serve_backend(&mut backend, Cursor::new(input), &mut output).unwrap();
    let mut frames = Vec::new();
    let mut reader = Cursor::new(output);
    while let Some(body) = read_frame_bytes(&mut reader, MAX_FRAME_BYTES).unwrap() {
        frames.push(serde_json::from_slice(&body).unwrap());
    }
    frames
}

fn sample(index: u64, host_ns: u64, pts: u64) -> FrameSample {
    FrameSample {
        index,
        host_ns,
        sample_pts_ns: pts,
        raw_hash: blake3_text(&index.to_le_bytes()),
    }
}

fn plan(masking: PixelMasking, codec: Codec) -> BackendPlan {
    BackendPlan {
        out_dir: PathBuf::from(OUT_DIR),
        window_id: 4242,
        pid: 77,
        fps: 30,
        scale: 2.0,
        audio: Vec::<TrackKind>::new(),
        visibility: VisibilityState::OffScreenVisible,
        masking,
        codec,
    }
}

fn full_take() -> Value {
    let region = Rect {
        x: 1.0,
        y: 2.0,
        width: 30.5,
        height: 40.0,
    };
    let start = plan(PixelMasking::Declared(vec![region]), Codec::Hevc);
    let requests = vec![
        request(1, "hello", json!({})),
        request(2, "sources", json!({})),
        request(3, "start", json!({ "plan": encode_plan(&start).unwrap() })),
        request(4, "stop", json!({})),
    ];
    let mut backend = FakeBackend::macos();
    backend.sources = vec![Source {
        window_id: 4242,
        pid: 77,
        app_id: "dev.rightkit.fixture".into(),
        title: "Fixture \"one\"".into(),
        bounds_pt: Rect {
            x: 3600.0,
            y: 0.0,
            width: 480.0,
            height: 270.0,
        },
        visibility: VisibilityState::OffScreenVisible,
    }];
    // Past 2^53, so a helper that routes integers through doubles fails here.
    let host = 9_007_199_254_740_993_u64;
    backend.push(vec![
        HelperEvent::Ready(sample(0, host, 0)),
        HelperEvent::Frame(sample(1, host + 33_333_333, 33_333_333)),
        HelperEvent::Dropped {
            host_ns: host + 100_000_000,
            count: 2,
            interval_ms: 66.66666666666667,
            reason: DropReason::Backpressure,
        },
        HelperEvent::Dropped {
            host_ns: host + 133_000_000,
            count: 1,
            interval_ms: 33.333333333333336,
            reason: DropReason::Encoder,
        },
        HelperEvent::Geometry {
            host_ns: host + 150_000_000,
            bounds_pt: Rect {
                x: 3600.0,
                y: 0.0,
                width: 640.0,
                height: 360.0,
            },
            scale: 2.0,
            dpi: 144,
        },
        HelperEvent::Failed(Failure::WindowHidden),
        HelperEvent::Failed(Failure::WindowMinimized),
        HelperEvent::Failed(Failure::TargetExited),
        HelperEvent::Failed(Failure::PermissionRevoked),
        HelperEvent::Failed(Failure::DiskFull),
        HelperEvent::Failed(Failure::MaskGeometryUnknown),
        HelperEvent::Failed(Failure::HelperLost),
        HelperEvent::Failed(Failure::Protocol("bad frame".into())),
        HelperEvent::Failed(Failure::Io("disk".into())),
        HelperEvent::Exited,
    ]);
    let frames = serve(&requests, backend);
    json!({ "name": "full_take", "requests": requests, "helperFrames": frames })
}

fn denied_and_unknown() -> Value {
    let start = plan(PixelMasking::CaptureSafeState, Codec::H264);
    let requests = vec![
        request(1, "hello", json!({})),
        request(2, "start", json!({ "plan": encode_plan(&start).unwrap() })),
        request(3, "frobnicate", json!({})),
        request(4, "abort", json!({})),
    ];
    let mut backend = FakeBackend::macos();
    backend.set_permission(Permission::Denied);
    let frames = serve(&requests, backend);
    json!({ "name": "denied_and_unknown", "requests": requests, "helperFrames": frames })
}

/// A start plan with the unknown-mask mode, for the plan decoder.
fn unknown_mask_plan() -> Value {
    encode_plan(&plan(PixelMasking::Unknown, Codec::H264)).unwrap()
}

/// Bytes `i % 251` for `len`, the BLAKE3 reference test pattern.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn build() -> Value {
    fs::create_dir_all(OUT_DIR).unwrap();
    let hashes: Vec<Value> = [
        0usize, 1, 3, 63, 64, 65, 127, 128, 1023, 1024, 1025, 2047, 2048, 2049, 3072, 3073, 4096,
        4097, 5120, 7168, 8192, 8193, 31_744, 65_536, 100_003, 1_048_576, 1_048_577, 1_440_000,
        5_760_000,
    ]
    .iter()
    .map(|&len| json!({ "len": len, "hash": blake3_text(&pattern(len)) }))
    .collect();

    let mut framed = Vec::new();
    let body = json!({ "kind": "bye" });
    write_frame(&mut framed, &body, MAX_FRAME_BYTES).unwrap();
    let hex: String = framed.iter().map(|b| format!("{b:02x}")).collect();

    json!({
        "comment": "Generated by rightkit-record macos::golden. Regenerate with RK_UPDATE_GOLDEN=1.",
        "blake3Pattern": "byte i = i % 251",
        "blake3": hashes,
        "framedBye": { "hex": hex, "body": body },
        "scenarios": [full_take(), denied_and_unknown()],
        "unknownMaskPlan": unknown_mask_plan(),
    })
}

#[test]
fn golden_vectors_match_the_rust_protocol() {
    let generated = build();
    let path = golden_path();
    if std::env::var_os("RK_UPDATE_GOLDEN").is_some() {
        let text = serde_json::to_string_pretty(&generated).unwrap() + "\n";
        fs::write(&path, text).unwrap();
        return;
    }
    let stored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); run with RK_UPDATE_GOLDEN=1",
            path.display()
        )
    }))
    .unwrap();
    assert_eq!(
        stored, generated,
        "golden vectors are stale; run with RK_UPDATE_GOLDEN=1 and rerun the Swift tests"
    );
}

#[test]
fn the_helper_scenarios_are_what_the_swift_tests_expect() {
    // Pins the shape the Swift tests index into: counts and the order of helper frames.
    let scenario = full_take();
    let frames = scenario["helperFrames"].as_array().unwrap();
    // 15 events, then hello, sources, start and stop responses, then bye.
    assert_eq!(frames.len(), 15 + 4 + 1);
    assert_eq!(frames[0]["event"]["type"], "ready");
    assert_eq!(frames[14]["event"]["type"], "exited");
    assert_eq!(frames[15]["result"]["os"], "macos");
    assert_eq!(frames[19]["kind"], "bye");
    let second = denied_and_unknown();
    let frames = second["helperFrames"].as_array().unwrap();
    // hello, start (refused), frobnicate (refused), abort, bye.
    assert_eq!(frames.len(), 5);
    assert_eq!(frames[1]["error"]["code"], "permission_denied");
    assert_eq!(frames[2]["error"]["code"], "arg_invalid");
}
