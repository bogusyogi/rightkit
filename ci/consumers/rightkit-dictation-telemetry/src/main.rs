// rightkit-dictation-telemetry consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no network, no files.
use rightkit_dictation_telemetry::feedback::{
    configure_feedback_endpoint, diagnostics_upload_headers, new_submission_id, submit_feedback,
    FeedbackEndpoint,
};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    let mut all = true;
    let (a, b) = (new_submission_id(), new_submission_id());
    all &= check(a.len() == 36 && a != b, "submission ids are fresh UUIDs");
    configure_feedback_endpoint(FeedbackEndpoint {
        url: "https://example.invalid/v1/feedback",
        header_prefix: "x-example-",
    });
    let headers = diagnostics_upload_headers("1.2.3", None, Some(&a));
    all &= check(
        headers
            .iter()
            .any(|(name, value)| name == "x-example-install-id" && value == &a),
        "upload headers carry the configured prefix",
    );
    all &= check(
        submit_feedback("   ", None, &a).is_err(),
        "empty feedback is refused before sending",
    );
    if !all {
        std::process::exit(1);
    }
}
