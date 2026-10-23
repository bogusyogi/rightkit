// rightkit-record 0.3.3 consumed by exact registry pin, outside the workspace.
//
// Proves the pure parts of the record core that a host calls with no capture backend: the id rule
// (plan::valid_id); the start-request refusals that happen before any capture is attempted
// (plan::validate: fps, cursor pixels, empty surface list); the content-hash family (hash::
// blake3_text, is_blake3_text, file_hash, keyed_digest); the stable error code strings
// (error::Code); and bundle_verify refusing a directory with no manifest and refusing a path that is
// not a directory (not_found).
// Does not prove: a complete bundle that verifies, the Recorder take lifecycle, any Backend, the
// ScreenCaptureKit or Windows helper, redaction keys, cursor assets, media recovery, BundleReader,
// or the success path of plan::validate (it needs a constructed Surface).
// State: files only under a fresh temp directory, removed at the end. No capture, no helper binary,
// no Screen Recording permission, no network, no real user data.
use std::fs;

use rightkit_record::error::Code;
use rightkit_record::hash::{blake3_text, file_hash, is_blake3_text, keyed_digest};
use rightkit_record::plan::{
    valid_id, validate, ScenarioSpec, ScenarioStep, StartRequest, TargetSpec,
};
use rightkit_record::verify::{bundle_verify, VerifyOptions};

fn scenario() -> ScenarioSpec {
    ScenarioSpec {
        scenario_id: "demo.one".into(),
        steps: vec![ScenarioStep {
            step_id: "step-1".into(),
            label: "Open".into(),
            say: None,
        }],
    }
}

/// A target with no surfaces. Every refusal this recipe checks is reached before the surface
/// check, except the surface check itself, which is the last one exercised.
fn target(window_id: u64) -> TargetSpec {
    TargetSpec {
        app_id: "demo-app".into(),
        window_id,
        pid: 7,
        surfaces: Vec::new(),
    }
}

/// The code and message of a start request that must be refused.
fn refusal(request: &StartRequest) -> (Code, String) {
    match validate(request) {
        Ok(()) => panic!("expected the start request to be refused"),
        Err(e) => (e.code, e.message),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("rightkit-record-consumer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;

    // Ids: letters, digits and _.:- , starting alphanumeric.
    assert!(valid_id("bundle.1-a_b:c"));
    assert!(!valid_id(""));
    assert!(!valid_id("-leading"));
    assert!(!valid_id("has space"));

    // Start-request refusals. The output directory is never reached, so nothing is created.
    let out = dir.join("take-1");
    let mut request = StartRequest::new(target(42), scenario(), out.clone());
    request.fps = 24;
    let (code, _) = refusal(&request);
    assert_eq!(code, Code::ArgInvalid, "fps must be 30 or 60");

    request.fps = 60;
    request.cursor = true;
    let (code, _) = refusal(&request);
    assert_eq!(code, Code::ArgInvalid, "cursor pixels are never recorded");

    request.cursor = false;
    request.target.window_id = 0;
    let (code, _) = refusal(&request);
    assert_eq!(code, Code::ArgInvalid, "window_id must be non-zero");

    request.target.window_id = 42;
    let (code, message) = refusal(&request);
    assert_eq!(code, Code::ArgInvalid);
    assert!(
        message.contains("surface"),
        "an empty surface list is refused: {message}"
    );
    assert!(
        !out.exists(),
        "a refused request creates no bundle directory"
    );

    // Content hashes: canonical blake3 text, streamed file hash, keyed digests per field and key.
    let frame = b"frame bytes";
    let digest = blake3_text(frame);
    assert!(is_blake3_text(&digest));
    assert!(!is_blake3_text("blake3:ABCD"));
    assert!(!is_blake3_text(&format!("sha256:{}", "a".repeat(64))));

    let clip = dir.join("clip.bin");
    fs::write(&clip, frame)?;
    let (hashed, size) = file_hash(&clip)?;
    assert_eq!(hashed, digest, "file hash equals the hash of its bytes");
    assert_eq!(size, frame.len() as u64);

    let keyed = keyed_digest(b"key-one", "target.label", "x");
    assert!(keyed.starts_with("hmac-sha256:"));
    assert!(!is_blake3_text(&keyed));
    assert_eq!(
        keyed,
        keyed_digest(b"key-one", "target.label", "x"),
        "deterministic"
    );
    assert_ne!(
        keyed,
        keyed_digest(b"key-one", "text", "x"),
        "field separates digests"
    );
    assert_ne!(
        keyed,
        keyed_digest(b"key-two", "target.label", "x"),
        "key separates digests"
    );

    // Error codes: stable strings, and a round trip through parse.
    assert_eq!(Code::parse(Code::OutExists.as_str()), Some(Code::OutExists));
    assert_eq!(Code::NotComplete.as_str(), "bundle_not_complete");
    assert_eq!(Code::VerifyFailed.as_str(), "bundle_verify_failed");
    assert_eq!(Code::parse("no_such_code"), None);

    // bundle_verify: a directory with no manifest fails verification without a panic.
    let empty = dir.join("bundle");
    fs::create_dir_all(&empty)?;
    let report = bundle_verify(&empty, &VerifyOptions::default())?;
    assert!(!report.ok(), "a directory with no manifest is not a bundle");
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.starts_with("manifest unreadable")),
        "the problem names the missing manifest: {:?}",
        report.problems
    );

    // bundle_verify: a path that is not a directory is refused as not_found.
    match bundle_verify(&dir.join("no-such-bundle"), &VerifyOptions::default()) {
        Ok(_) => panic!("a missing path is not a bundle directory"),
        Err(e) => assert_eq!(e.code, Code::NotFound),
    }

    fs::remove_dir_all(&dir)?;
    println!("OK rightkit-record 0.3.3 (registry consumer)");
    Ok(())
}
