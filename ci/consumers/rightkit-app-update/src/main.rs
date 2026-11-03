// rightkit-app-update 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Probation runs its real sentinel state machine against one JSON file in a private temporary
// directory that is removed at the end. The fetch bridge is only driven through refusals that happen
// before any connection: no DNS lookup, network, process or user data is touched.
use rightkit_app_update::{
    BootDecision, BootProbation, FetchBridge, FetchErrorCode, FetchPolicy, FetchRequest,
    ProbationError, Sentinel, DEFAULT_MAX_BOOT_ATTEMPTS, GUIDANCE_REINSTALL,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn request(json: &str) -> FetchRequest {
    match serde_json::from_str(json) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("FAIL: request json: {error}");
            std::process::exit(1);
        }
    }
}

fn probation() {
    check(
        "default attempts",
        DEFAULT_MAX_BOOT_ATTEMPTS == 2 && GUIDANCE_REINSTALL == "reinstall",
    );
    let dir = std::env::temp_dir().join(format!(
        "rightkit-app-update-consumer-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private temp directory");
    let path = dir.join("boot-sentinel.json");
    let probation = BootProbation::new(&path);
    check("path is kept", probation.path() == path.as_path());

    // Nothing on probation yet.
    check(
        "no sentinel means no probation",
        matches!(probation.evaluate_boot("2.0.0"), Ok(BootDecision::None)),
    );

    // Input refusals happen before anything is written.
    check(
        "empty version refused",
        matches!(
            probation.begin("", "2.0.0", None),
            Err(ProbationError::Invalid(_))
        ),
    );
    check(
        "previous-copy install path refused",
        matches!(
            probation.begin("1.0.0", "2.0.0", Some("/Applications/App.previous")),
            Err(ProbationError::Invalid(_))
        ),
    );
    check(
        "root install path refused",
        matches!(
            probation.begin("1.0.0", "2.0.0", Some("/")),
            Err(ProbationError::Invalid(_))
        ),
    );
    check(
        "bad stage name refused",
        matches!(
            probation.report_stage("not a stage"),
            Err(ProbationError::Invalid(_))
        ),
    );
    check("nothing was written", !path.exists());
    check(
        "zero attempts refused",
        matches!(
            probation.clone().max_boot_attempts(0),
            Err(ProbationError::Invalid(_))
        ),
    );

    // An update begins: the first starts are on probation and count up.
    probation
        .begin("1.0.0", "2.0.0", Some("/Applications/App.app"))
        .expect("begin writes the sentinel");
    check("sentinel file exists", path.is_file());
    let first = probation.evaluate_boot("2.0.0").expect("first boot");
    check(
        "first boot is on probation",
        first
            == BootDecision::Probation {
                to_version: "2.0.0".into(),
                boot_attempts: 1,
            },
    );
    probation
        .report_stage("webview-ready")
        .expect("stage is recorded");
    let wire: Sentinel = serde_json::from_slice(&std::fs::read(&path).expect("sentinel readable"))
        .expect("sentinel is the documented record");
    check(
        "sentinel record",
        wire.from_version == "1.0.0"
            && wire.to_version == "2.0.0"
            && wire.boot_attempts == 1
            && wire.last_stage.as_deref() == Some("webview-ready")
            && wire.install_path.as_deref() == Some("/Applications/App.app"),
    );
    check(
        "second boot is still on probation",
        probation.evaluate_boot("2.0.0").expect("second boot")
            == BootDecision::Probation {
                to_version: "2.0.0".into(),
                boot_attempts: 2,
            },
    );

    // The third start without an acknowledgement declares the boot failed and clears the sentinel.
    match probation.evaluate_boot("2.0.0").expect("third boot") {
        BootDecision::ReinstallRequired(failure) => check(
            "failed boot names the release to reinstall",
            failure.failed_version == "2.0.0"
                && failure.from_version == "1.0.0"
                && failure.boot_attempts == 3
                && failure.guidance() == GUIDANCE_REINSTALL,
        ),
        other => {
            eprintln!("FAIL: expected reinstall guidance, got {other:?}");
            std::process::exit(1);
        }
    }
    check("failed boot clears the sentinel", !path.exists());

    // A healthy acknowledgement clears the sentinel, and a superseded sentinel is stale.
    probation
        .begin("2.0.0", "3.0.0", None)
        .expect("second update");
    probation.acknowledge_healthy().expect("acknowledge");
    check("acknowledged sentinel is gone", !path.exists());
    probation
        .begin("2.0.0", "3.0.0", None)
        .expect("third update");
    check(
        "another running version is stale",
        probation.evaluate_boot("4.0.0").expect("stale boot") == BootDecision::Stale,
    );
    check("stale sentinel is cleared", !path.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

fn fetch_bridge() {
    // The policy refuses an empty or malformed host list.
    check(
        "empty host list refused",
        FetchPolicy::new(Vec::<String>::new()).err().map(|e| e.code)
            == Some(FetchErrorCode::Invalid),
    );
    check(
        "host with a scheme refused",
        FetchPolicy::new(["https://example.com"])
            .err()
            .map(|e| e.code)
            == Some(FetchErrorCode::Invalid),
    );
    let policy = FetchPolicy::new(["Example.COM."])
        .expect("valid host")
        .path_prefix("example.com", "/api")
        .max_request_bytes(1024)
        .max_response_bytes(4096)
        .timeout(std::time::Duration::from_secs(5));
    let bridge = FetchBridge::new(policy);

    let refusal = |json: &str, expected: FetchErrorCode, name: &str| {
        let got = bridge.fetch(request(json)).err().map(|e| e.code);
        check(name, got == Some(expected));
    };
    refusal(
        r#"{"url":"http://example.com/api"}"#,
        FetchErrorCode::Refused,
        "plain http refused",
    );
    refusal(
        r#"{"url":"https://user:pw@example.com/api"}"#,
        FetchErrorCode::Refused,
        "userinfo refused",
    );
    refusal(
        r#"{"url":"https://example.com/api","method":"TRACE"}"#,
        FetchErrorCode::Invalid,
        "unknown method refused",
    );
    refusal(
        r#"{"url":"https://example.com/api","headers":[["Cookie","a=b"]]}"#,
        FetchErrorCode::Invalid,
        "cookie header refused",
    );
    refusal(
        r#"{"url":"https://example.com/api","headers":[["X-A","a\r\nb: c"]]}"#,
        FetchErrorCode::Invalid,
        "header injection refused",
    );
    refusal(
        r#"{"url":"https://example.com/api","bodyBase64":"aGk="}"#,
        FetchErrorCode::Invalid,
        "body on GET refused",
    );
    refusal(
        r#"{"url":"https://example.com/api","method":"POST","bodyBase64":"!!!!"}"#,
        FetchErrorCode::Invalid,
        "malformed base64 refused",
    );
    let too_big = format!(
        r#"{{"url":"https://example.com/api","method":"POST","bodyBase64":"{}"}}"#,
        "QUFB".repeat(1000)
    );
    refusal(&too_big, FetchErrorCode::TooLarge, "oversized body refused");

    // The error is a closed code with fixed text on the wire.
    let error = bridge
        .fetch(request(r#"{"url":"http://example.com/"}"#))
        .err()
        .expect("refusal");
    let wire = serde_json::to_value(error).expect("error serialises");
    check(
        "error wire",
        wire["code"] == "refused" && wire["message"] == "The request is not allowed for this app.",
    );
}

fn main() {
    probation();
    fetch_bridge();
    println!("OK rightkit-app-update 0.1.0 (registry consumer, public API)");
}
