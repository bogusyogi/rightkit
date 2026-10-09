//! Port of `build-control-broker/src/ownership-admission.test.mjs`. Each Node `test(...)`
//! is one `#[test]` with its name in snake_case. Rust-only additions are at the end.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::clock::{Clock, ManualClock};
use serde_json::{json, Map, Value};

use crate::admission::{
    classify_ownership_report, AdmissionConfig, Mode, OwnershipAdmission, Target, Violation,
};
use crate::limits::WorkerLimits;
use crate::runner::{default_scanner_root, resolve_lexical, FnScanRunner, ScanRequest};
use crate::verdict::{codes, ownership_verdict};

const IDENTITY: &str = "https://github.com/test/repo";

fn digest(n: u64) -> String {
    format!("{n:064x}")
}

fn root() -> String {
    std::env::temp_dir()
        .join("ownership-verdict-app")
        .to_string_lossy()
        .into_owned()
}

fn trust() -> Value {
    json!({
        "schema": 1,
        "revokedKeyIds": [],
        "verifiedTrackingEvidence": [],
        "minimumScannerVersion": "2.0.0",
        "minimumCatalogVersion": "1",
        "repositories": [{"repositoryIdentity": IDENTITY, "roots": ["."], "visibility": "private"}]
    })
}

fn inputs() -> Value {
    json!({
        "contentDigest": digest(1),
        "catalogDigest": digest(2),
        "trustDigest": digest(3),
        "approvalsDigest": digest(4),
        "scannerPolicyDigest": digest(5),
        "protectedFloors": {"minimumScannerVersion": "2.0.0", "minimumCatalogVersion": "1", "minimumCatalogDigest": null}
    })
}

fn violation() -> Value {
    json!({
        "fingerprint": "browser.npm.playwright",
        "capability": "browser-automation",
        "package": "rightkit-browser",
        "file": "package.json",
        "line": 0
    })
}

fn metadata() -> Value {
    json!({
        "repositoryIdentity": IDENTITY,
        "scannerVersion": "2.4.0",
        "catalogVersion": "1",
        "catalogDigest": digest(2),
        "coverage": [],
        "roots": [{"root": "."}]
    })
}

fn report() -> Value {
    json!({"ok": true, "enrollmentPending": false, "violations": [], "problems": [], "coverage": [], "metadata": metadata()})
}

/// Node's `{...base, ...patch}`: shallow merge of top-level keys.
fn with(mut base: Value, patch: Value) -> Value {
    if let (Some(target), Some(source)) = (base.as_object_mut(), patch.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    base
}

fn report_with(patch: Value) -> Value {
    with(report(), patch)
}

fn meta_with(patch: Value) -> Value {
    with(metadata(), patch)
}

fn repository_target(root: &str) -> Target<'_> {
    Target {
        repository_root: Some(root),
        workspace_root: None,
    }
}

fn gate(
    mode: Mode,
    inputs_fn: impl Fn(&ScanRequest<'_>) -> Value + Send + Sync + 'static,
    scan_fn: impl Fn(&ScanRequest<'_>) -> Value + Send + Sync + 'static,
) -> OwnershipAdmission {
    gate_with_clock(mode, Arc::new(ManualClock::new(0)), inputs_fn, scan_fn)
}

fn gate_with_clock(
    mode: Mode,
    clock: Arc<dyn Clock>,
    inputs_fn: impl Fn(&ScanRequest<'_>) -> Value + Send + Sync + 'static,
    scan_fn: impl Fn(&ScanRequest<'_>) -> Value + Send + Sync + 'static,
) -> OwnershipAdmission {
    OwnershipAdmission::new(AdmissionConfig {
        mode,
        timeout: Duration::from_secs(30),
        scanner_root: Some(PathBuf::from("staged-scanner")),
        clock,
        inputs_runner: Arc::new(FnScanRunner(inputs_fn)),
        scan_runner: Arc::new(FnScanRunner(scan_fn)),
        limits: WorkerLimits::default(),
    })
}

/// Node `admission(mode, value)`: fixed inputs, and the given report from the scan.
fn admission(mode: Mode, value: Value) -> OwnershipAdmission {
    gate(
        mode,
        |_| json!({"inputs": inputs(), "trust": trust(), "complete": true}),
        move |_| json!({"report": value.clone(), "trust": trust(), "inputs": inputs()}),
    )
}

fn enforce_refuses_and_report_records(value: Value, code: &str) {
    let root = root();
    let enforce = admission(Mode::Enforce, value.clone());
    let advisory = admission(Mode::Report, value);
    let result = enforce.evaluate(repository_target(&root));
    assert!(!result.admit);
    assert_eq!(result.code.as_deref(), Some(code));
    assert!(!result.problems.is_empty());
    assert_eq!(enforce.assert_admitted(&result).unwrap_err().code, code);
    let recorded = advisory.evaluate(repository_target(&root));
    assert_eq!(recorded.code.as_deref(), Some(code));
    assert!(!recorded.admit);
    assert!(advisory.assert_admitted(&recorded).is_ok());
}

#[test]
fn enforce_refuses_ok_false_report_records_identical_typed_verdict() {
    enforce_refuses_and_report_records(report_with(json!({"ok": false})), codes::REPORT_FAILED);
}

#[test]
fn enforce_refuses_pending_enrollment_report_records_identical_typed_verdict() {
    enforce_refuses_and_report_records(
        report_with(json!({"ok": false, "enrollmentPending": true})),
        codes::PENDING_ENROLLMENT,
    );
}

#[test]
fn enforce_refuses_incomplete_coverage_report_records_identical_typed_verdict() {
    let value = report_with(
        json!({"coverage": [{"path": "source.bin", "status": "unscanned", "reason": "unsupported-input"}]}),
    );
    enforce_refuses_and_report_records(value, codes::INCOMPLETE_COVERAGE);
}

#[test]
fn enforce_refuses_invalid_registration_report_records_identical_typed_verdict() {
    let value = report_with(
        json!({"metadata": meta_with(json!({"repositoryIdentity": "https://github.com/test/unregistered"}))}),
    );
    enforce_refuses_and_report_records(value, codes::INVALID_REGISTRATION);
}

#[test]
fn enforce_refuses_scanner_downgrade_report_records_identical_typed_verdict() {
    let value = report_with(json!({"metadata": meta_with(json!({"scannerVersion": "1.0.0"}))}));
    enforce_refuses_and_report_records(value, codes::POLICY_DOWNGRADE);
}

#[test]
fn enforce_refuses_catalog_downgrade_report_records_identical_typed_verdict() {
    let value = report_with(json!({"metadata": meta_with(json!({"catalogVersion": "0"}))}));
    enforce_refuses_and_report_records(value, codes::POLICY_DOWNGRADE);
}

#[test]
fn enforce_refuses_violation_report_records_identical_typed_verdict() {
    enforce_refuses_and_report_records(
        report_with(json!({"ok": false, "violations": [violation()]})),
        codes::VIOLATION,
    );
}

#[test]
fn enforce_refuses_problems_report_records_identical_typed_verdict() {
    enforce_refuses_and_report_records(
        report_with(json!({"ok": false, "problems": ["exception expired"]})),
        codes::UNVERIFIED,
    );
}

#[test]
fn missing_extra_wrong_typed_and_non_object_reports_fail_closed() {
    let mut malformed: Vec<Value> = vec![
        Value::Null,
        json!([]),
        json!("clean"),
        json!(1),
        with(report(), json!({"extra": true})),
    ];
    for key in [
        "ok",
        "enrollmentPending",
        "violations",
        "problems",
        "coverage",
        "metadata",
    ] {
        let mut missing = report();
        missing.as_object_mut().unwrap().remove(key);
        malformed.push(missing);
        let wrong = if key == "ok" || key == "enrollmentPending" {
            json!("false")
        } else {
            json!(1)
        };
        let mut patch = Map::new();
        patch.insert(key.to_string(), wrong);
        malformed.push(report_with(Value::Object(patch)));
    }
    malformed.push(report_with(
        json!({"metadata": meta_with(json!({"extra": true}))}),
    ));
    malformed.push(report_with(json!({"coverage": [null]})));
    malformed.push(report_with(json!({"violations": [null]})));
    malformed.push(report_with(json!({"problems": [1]})));
    malformed.push(report_with(
        json!({"violations": [with(violation(), json!({"extra": true}))]}),
    ));
    malformed.push(report_with(json!({"rows": [{"capability": 1}]})));
    malformed.push(report_with(json!({"metadata": meta_with(json!({"contentDigests": [{"path": "source.rs", "digest": 1}]}))})));
    let root = root();
    for value in malformed {
        let gate = admission(Mode::Enforce, value.clone());
        let result = gate.evaluate(repository_target(&root));
        assert_eq!(
            result.code.as_deref(),
            Some(codes::MALFORMED_REPORT),
            "{value}"
        );
        assert_eq!(
            gate.assert_admitted(&result).unwrap_err().code,
            codes::MALFORMED_REPORT
        );
    }
}

#[test]
fn shared_classification_accepts_complete_protected_report_and_preserves_violation_details() {
    assert!(classify_ownership_report(&report(), &trust()).admit);
    let dirty =
        classify_ownership_report(&report_with(json!({"violations": [violation()]})), &trust());
    assert_eq!(dirty.code, codes::VIOLATION);
    assert_eq!(
        dirty.violations[0],
        Violation {
            capability: json!("browser-automation"),
            owner: json!("rightkit-browser"),
            fingerprint: json!("browser.npm.playwright"),
            file: json!("package.json"),
            line: json!(0),
        }
    );
    assert_eq!(
        classify_ownership_report(
            &report_with(json!({"enrollmentPending": true})),
            &Value::Null
        )
        .outcome,
        "pending-enrollment"
    );
    assert_eq!(
        classify_ownership_report(&report(), &Value::Null).code,
        codes::INVALID_REGISTRATION
    );
}

#[test]
fn catalog_paths_never_confer_owner_exemption_and_missing_roots_refuse() {
    let host = tempfile::tempdir().unwrap();
    let host_path = host.path();
    fs::create_dir_all(
        host_path
            .join("tools")
            .join("rightkit")
            .join("packages")
            .join("release"),
    )
    .unwrap();
    fs::write(
        host_path
            .join("tools")
            .join("rightkit")
            .join("packages")
            .join("release")
            .join("rightkit-ownership.json"),
        "{}",
    )
    .unwrap();
    fs::write(host_path.join("rightkit-ownership.json"), "{}").unwrap();
    let host_root = host_path.to_string_lossy().into_owned();
    let other_workspace = host_path
        .join("tools")
        .join("other")
        .to_string_lossy()
        .into_owned();

    let gate = admission(
        Mode::Enforce,
        report_with(json!({"violations": [violation()]})),
    );
    let result = gate.evaluate(repository_target(&host_root));
    assert_eq!(result.scan_root.as_deref(), Some(host_root.as_str()));
    assert_eq!(
        gate.assert_admitted(&result).unwrap_err().code,
        codes::VIOLATION
    );

    let with_workspace = gate.evaluate(Target {
        repository_root: Some(&host_root),
        workspace_root: Some(&other_workspace),
    });
    assert_eq!(
        with_workspace.scan_root.as_deref(),
        Some(host_root.as_str())
    );

    let missing = gate.evaluate(Target::default());
    assert_eq!(
        gate.assert_admitted(&missing).unwrap_err().code,
        codes::INVALID_REGISTRATION
    );
}

fn cache_invalidates_changed_binding(binding: &str) {
    let current = Arc::new(Mutex::new(inputs()));
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs_state = Arc::clone(&current);
    let scan_state = Arc::clone(&current);
    let scan_calls = Arc::clone(&calls);
    let gate = gate(
        Mode::Enforce,
        move |_| json!({"inputs": inputs_state.lock().unwrap().clone(), "trust": trust(), "complete": true}),
        move |_| {
            let call = scan_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let report_value = if call > 1 {
                report_with(json!({"ok": false, "violations": [violation()]}))
            } else {
                report()
            };
            json!({"report": report_value, "trust": trust(), "inputs": scan_state.lock().unwrap().clone()})
        },
    );
    let root = root();
    assert!(gate.evaluate(repository_target(&root)).admit);
    {
        let mut guard = current.lock().unwrap();
        if binding == "protectedFloors" {
            let mut floors = guard["protectedFloors"].clone();
            floors["minimumScannerVersion"] = json!("2.1.0");
            guard["protectedFloors"] = floors;
        } else {
            guard[binding] = json!(digest(99));
        }
    }
    let next = gate.evaluate(repository_target(&root));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!next.cached);
    assert!(!next.admit);
}

#[test]
fn cache_invalidates_changed_content_digest_without_ttl_reuse() {
    cache_invalidates_changed_binding("contentDigest");
}

#[test]
fn cache_invalidates_changed_catalog_digest_without_ttl_reuse() {
    cache_invalidates_changed_binding("catalogDigest");
}

#[test]
fn cache_invalidates_changed_trust_digest_without_ttl_reuse() {
    cache_invalidates_changed_binding("trustDigest");
}

#[test]
fn cache_invalidates_changed_approvals_digest_without_ttl_reuse() {
    cache_invalidates_changed_binding("approvalsDigest");
}

#[test]
fn cache_invalidates_changed_scanner_policy_digest_without_ttl_reuse() {
    cache_invalidates_changed_binding("scannerPolicyDigest");
}

#[test]
fn cache_invalidates_changed_protected_floors_without_ttl_reuse() {
    cache_invalidates_changed_binding("protectedFloors");
}

#[test]
fn identical_digests_share_cache_and_in_flight_scan_regardless_elapsed_time() {
    let clock = ManualClock::new(0);
    let calls = Arc::new(AtomicUsize::new(0));
    let probes = Arc::new(AtomicUsize::new(0));
    let probe_count = Arc::clone(&probes);
    let scan_count = Arc::clone(&calls);
    let gate = gate_with_clock(
        Mode::Enforce,
        Arc::new(clock.clone()),
        move |_| {
            probe_count.fetch_add(1, Ordering::SeqCst);
            json!({"inputs": inputs(), "trust": trust(), "complete": true})
        },
        move |_| {
            scan_count.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            json!({"report": report(), "trust": trust(), "inputs": inputs()})
        },
    );
    let root = root();
    let (first, second) = thread::scope(|scope| {
        let a = scope.spawn(|| gate.evaluate(repository_target(&root)));
        let b = scope.spawn(|| gate.evaluate(repository_target(&root)));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(first.admit);
    assert!(second.admit);
    clock.set(120_000);
    let third = gate.evaluate(repository_target(&root));
    assert!(third.cached);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(probes.load(Ordering::SeqCst) >= 3);
}

#[test]
fn incomplete_inventory_never_inherits_cached_or_in_flight_clean_verdict() {
    let complete = Arc::new(AtomicBool::new(true));
    let probe_complete = Arc::clone(&complete);
    let gate = gate(
        Mode::Enforce,
        move |_| json!({"inputs": inputs(), "trust": trust(), "complete": probe_complete.load(Ordering::SeqCst)}),
        |_| json!({"report": report(), "trust": trust(), "inputs": inputs()}),
    );
    let root = root();
    assert!(gate.evaluate(repository_target(&root)).admit);
    complete.store(false, Ordering::SeqCst);
    let refused = gate.evaluate(repository_target(&root));
    assert_eq!(refused.code.as_deref(), Some(codes::INCOMPLETE_COVERAGE));
    assert!(!refused.admit);
    assert!(!refused.cached);
}

#[test]
fn protected_pinned_catalog_root_coverage_and_malformed_registry_fail_closed() {
    let pinned = with(trust(), json!({"minimumCatalogDigest": digest(99)}));
    assert_eq!(
        classify_ownership_report(&report(), &pinned).code,
        codes::POLICY_DOWNGRADE
    );

    let repository = trust()["repositories"][0].clone();
    let engine_app = with(
        trust(),
        json!({"repositories": [with(repository, json!({"roots": ["engine", "app"]}))]}),
    );
    assert_eq!(
        classify_ownership_report(&report(), &engine_app).code,
        codes::INVALID_REGISTRATION
    );

    let null_entry = with(trust(), json!({"repositories": [null]}));
    assert_eq!(
        classify_ownership_report(&report(), &null_entry).code,
        codes::INVALID_REGISTRATION
    );

    let incomplete = report_with(
        json!({"metadata": meta_with(json!({"coverage": [{"path": "source.rs", "status": "incomplete", "reason": "missing"}]}))}),
    );
    assert_eq!(
        classify_ownership_report(&incomplete, &trust()).code,
        codes::INCOMPLETE_COVERAGE
    );
}

#[test]
fn missing_digests_changed_scan_bindings_and_unavailable_scanners_refuse_failures_retry() {
    let root = root();
    for binding in [
        "contentDigest",
        "catalogDigest",
        "trustDigest",
        "approvalsDigest",
        "scannerPolicyDigest",
        "protectedFloors",
    ] {
        let mut incomplete = inputs();
        incomplete.as_object_mut().unwrap().remove(binding);
        let gate = gate(
            Mode::Enforce,
            move |_| json!({"inputs": incomplete.clone(), "trust": trust(), "complete": true}),
            |_| json!({"report": report(), "trust": trust(), "inputs": inputs()}),
        );
        let result = gate.evaluate(repository_target(&root));
        assert_eq!(
            gate.assert_admitted(&result).unwrap_err().code,
            codes::INVALID_INPUTS,
            "{binding}"
        );
    }

    let changed_scan = gate(
        Mode::Enforce,
        |_| json!({"inputs": inputs(), "trust": trust(), "complete": true}),
        |_| json!({"report": report(), "trust": trust(), "inputs": with(inputs(), json!({"contentDigest": digest(99)}))}),
    );
    let changed = changed_scan.evaluate(repository_target(&root));
    assert_eq!(
        changed_scan.assert_admitted(&changed).unwrap_err().code,
        codes::INPUTS_CHANGED
    );

    for complete in [None, Some(Value::Null), Some(json!("true"))] {
        let malformed = gate(
            Mode::Enforce,
            move |_| {
                let mut observed = json!({"inputs": inputs(), "trust": trust()});
                if let Some(value) = &complete {
                    observed["complete"] = value.clone();
                }
                observed
            },
            |_| json!({"report": report(), "trust": trust(), "inputs": inputs()}),
        );
        assert_eq!(
            malformed.evaluate(repository_target(&root)).code.as_deref(),
            Some(codes::INVALID_INPUTS)
        );
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let scan_calls = Arc::clone(&calls);
    let broken = gate(
        Mode::Enforce,
        |_| json!({"inputs": inputs(), "trust": trust(), "complete": true}),
        move |_| {
            scan_calls.fetch_add(1, Ordering::SeqCst);
            json!({"error": "scan timeout"})
        },
    );
    for _ in 0..2 {
        let result = broken.evaluate(repository_target(&root));
        assert_eq!(
            broken.assert_admitted(&result).unwrap_err().code,
            codes::UNVERIFIED
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let unstaged = OwnershipAdmission::new(AdmissionConfig {
        mode: Mode::Enforce,
        timeout: Duration::from_secs(30),
        scanner_root: None,
        clock: Arc::new(ManualClock::new(0)),
        inputs_runner: Arc::new(FnScanRunner(|_: &ScanRequest<'_>| -> Value {
            panic!("unstaged scanner must not run")
        })),
        scan_runner: Arc::new(FnScanRunner(|_: &ScanRequest<'_>| -> Value {
            panic!("unstaged scanner must not run")
        })),
        limits: WorkerLimits::default(),
    });
    assert_eq!(
        unstaged.evaluate(repository_target(&root)).outcome,
        "unscanned"
    );

    let off = OwnershipAdmission::new(AdmissionConfig {
        mode: Mode::Off,
        timeout: Duration::from_secs(30),
        scanner_root: Some(PathBuf::from("staged-scanner")),
        clock: Arc::new(ManualClock::new(0)),
        inputs_runner: Arc::new(FnScanRunner(|_: &ScanRequest<'_>| -> Value {
            panic!("off must not probe")
        })),
        scan_runner: Arc::new(FnScanRunner(|_: &ScanRequest<'_>| -> Value {
            panic!("off scans")
        })),
        limits: WorkerLimits::default(),
    });
    assert_eq!(off.evaluate(repository_target(&root)).outcome, "off");
}

#[test]
fn only_block_tier_or_banned_violations_refuse_advise_violations_admit_with_an_advisory_verdict() {
    let advise = with(violation(), json!({"tier": "advise"}));
    let advisory = ownership_verdict(
        &report_with(json!({"violations": [advise.clone()]})),
        &trust(),
    );
    assert!(advisory.admit);
    assert_eq!(advisory.code, codes::ADVISORY);
    assert!(advisory.problems[0].contains("[advise]"));
    let blocked = ownership_verdict(
        &report_with(
            json!({"ok": false, "violations": [with(violation(), json!({"tier": "block"}))]}),
        ),
        &trust(),
    );
    assert_eq!(blocked.code, codes::VIOLATION);
    let banned = ownership_verdict(
        &report_with(
            json!({"ok": false, "violations": [with(advise, json!({"policy": "banned"}))]}),
        ),
        &trust(),
    );
    assert_eq!(banned.code, codes::VIOLATION);
    let untiered = ownership_verdict(
        &report_with(json!({"ok": false, "violations": [violation()]})),
        &trust(),
    );
    assert_eq!(
        untiered.code,
        codes::VIOLATION,
        "missing tier fails closed as block"
    );
    let warn = ownership_verdict(
        &report_with(json!({"violations": [with(violation(), json!({"tier": "warn"}))]})),
        &trust(),
    );
    assert_eq!(warn.code, codes::MALFORMED_REPORT);
}

// ---- Rust-only coverage: bounded workers, scanner discovery and the verdict's authority. ----

#[test]
fn inventory_workers_respect_global_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let probe_active = Arc::clone(&active);
    let probe_peak = Arc::clone(&peak);
    let scan_active = Arc::clone(&active);
    let scan_peak = Arc::clone(&peak);
    let config = AdmissionConfig {
        mode: Mode::Enforce,
        timeout: Duration::from_secs(30),
        scanner_root: Some(PathBuf::from("staged-scanner")),
        clock: Arc::new(ManualClock::new(0)),
        inputs_runner: Arc::new(FnScanRunner(move |_: &ScanRequest<'_>| {
            let now = probe_active.fetch_add(1, Ordering::SeqCst) + 1;
            probe_peak.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(15));
            probe_active.fetch_sub(1, Ordering::SeqCst);
            json!({"inputs": inputs(), "trust": trust(), "complete": true})
        })),
        scan_runner: Arc::new(FnScanRunner(move |_: &ScanRequest<'_>| {
            let now = scan_active.fetch_add(1, Ordering::SeqCst) + 1;
            scan_peak.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(15));
            scan_active.fetch_sub(1, Ordering::SeqCst);
            json!({"report": report(), "trust": trust(), "inputs": inputs()})
        })),
        limits: WorkerLimits::new(1, 1, Duration::from_secs(10)).unwrap(),
    };
    let gate = Arc::new(OwnershipAdmission::new(config));
    let handles: Vec<_> = ["/repo/a", "/repo/b", "/repo/c"]
        .into_iter()
        .map(|repo| {
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                gate.evaluate(Target {
                    repository_root: Some(repo),
                    workspace_root: None,
                })
            })
        })
        .collect();
    for handle in handles {
        assert!(handle.join().unwrap().admit);
    }
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

#[test]
fn inventory_worker_wait_is_bounded_and_refuses() {
    let config = AdmissionConfig {
        mode: Mode::Enforce,
        timeout: Duration::from_secs(30),
        scanner_root: Some(PathBuf::from("staged-scanner")),
        clock: Arc::new(ManualClock::new(0)),
        inputs_runner: Arc::new(FnScanRunner(|_: &ScanRequest<'_>| {
            thread::sleep(Duration::from_millis(400));
            json!({"inputs": inputs(), "trust": trust(), "complete": true})
        })),
        scan_runner: Arc::new(FnScanRunner(
            |_: &ScanRequest<'_>| json!({"report": report(), "trust": trust(), "inputs": inputs()}),
        )),
        limits: WorkerLimits::new(1, 1, Duration::from_millis(50)).unwrap(),
    };
    let gate = Arc::new(OwnershipAdmission::new(config));
    let holder = {
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            gate.evaluate(Target {
                repository_root: Some("/repo/held"),
                workspace_root: None,
            })
        })
    };
    thread::sleep(Duration::from_millis(50));
    let refused = gate.evaluate(Target {
        repository_root: Some("/repo/waiting"),
        workspace_root: None,
    });
    assert!(!refused.admit);
    assert_eq!(refused.code.as_deref(), Some(codes::UNVERIFIED));
    assert!(
        refused.problems[0].contains("capacity unavailable"),
        "{:?}",
        refused.problems
    );
    assert!(holder.join().unwrap().admit);
}

#[test]
fn default_scanner_root_requires_worker_and_catalog() {
    let base = tempfile::tempdir().unwrap();
    let release = base.path().join("release");
    fs::create_dir_all(&release).unwrap();
    fs::write(release.join("ownership-scan-worker.mjs"), "").unwrap();
    let here = base.path().join("lib").join("broker");
    assert_eq!(default_scanner_root(&here), None, "catalog is required");
    fs::write(release.join("rightkit-ownership.json"), "{}").unwrap();
    assert_eq!(default_scanner_root(&here), Some(resolve_lexical(&release)));
}

#[test]
fn refusal_reason_carries_code_and_problems() {
    let gate = admission(
        Mode::Enforce,
        report_with(json!({"ok": false, "violations": [violation()]})),
    );
    let result = gate.evaluate(repository_target(&root()));
    let reason = result.refusal_reason();
    assert!(reason.starts_with("RK_OWNERSHIP_VIOLATION: "), "{reason}");
    assert!(reason.contains("browser-automation"));
}

/// A clean verdict that relied on an exception must not outlive the exception's
/// expiry day: the same inputs on the next UTC day force a fresh scan.
#[test]
fn cached_clean_verdict_does_not_survive_into_the_next_utc_day() {
    let calls = Arc::new(AtomicUsize::new(0));
    let scan_calls = Arc::clone(&calls);
    let clock = ManualClock::new(1_791_504_000_000 + 23 * 3_600_000); // 2026-10-09 23:00Z
    let gate = gate_with_clock(
        Mode::Enforce,
        Arc::new(clock.clone()),
        move |_| json!({"inputs": inputs(), "trust": trust(), "complete": true}),
        move |_| {
            let call = scan_calls.fetch_add(1, Ordering::SeqCst) + 1;
            // Day two: the exception has expired, so the scan now reports a violation.
            let report_value = if call > 1 {
                report_with(json!({"ok": false, "violations": [violation()]}))
            } else {
                report()
            };
            json!({"report": report_value, "trust": trust(), "inputs": inputs()})
        },
    );
    let root = root();
    assert!(gate.evaluate(repository_target(&root)).admit);
    assert!(
        gate.evaluate(repository_target(&root)).cached,
        "same day reuses the verdict"
    );
    clock.advance(2 * 3_600_000); // 2026-10-10 01:00Z
    let next = gate.evaluate(repository_target(&root));
    assert!(!next.cached);
    assert!(!next.admit);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// A follower's inputs probe must not queue behind the leader's running scan, so it
/// discovers the same key and joins the in-flight scan instead of waiting out the scan or
/// starting its own. Probe and scan permits are separate per repository.
#[test]
fn follower_probe_runs_during_leader_scan_and_shares_the_in_flight_scan() {
    let probes = Arc::new(AtomicUsize::new(0));
    let scans = Arc::new(AtomicUsize::new(0));
    let (probe_count, scan_count, scan_probes) =
        (Arc::clone(&probes), Arc::clone(&scans), Arc::clone(&probes));
    let gate = Arc::new(gate(
        Mode::Enforce,
        move |_| {
            probe_count.fetch_add(1, Ordering::SeqCst);
            json!({"inputs": inputs(), "trust": trust(), "complete": true})
        },
        move |_| {
            scan_count.fetch_add(1, Ordering::SeqCst);
            // Hold the scan open until the follower's probe has completed.
            let started = std::time::Instant::now();
            while scan_probes.load(Ordering::SeqCst) < 2 {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "follower probe queued behind the scan"
                );
                thread::sleep(Duration::from_millis(1));
            }
            thread::sleep(Duration::from_millis(50));
            json!({"report": report(), "trust": trust(), "inputs": inputs()})
        },
    ));
    let root = root();
    let leader = {
        let (gate, root) = (Arc::clone(&gate), root.clone());
        thread::spawn(move || gate.evaluate(repository_target(&root)))
    };
    while scans.load(Ordering::SeqCst) == 0 {
        thread::sleep(Duration::from_millis(1));
    }
    let follower = gate.evaluate(repository_target(&root));
    let leader = leader.join().unwrap();
    assert!(leader.admit && follower.admit);
    assert_eq!(
        scans.load(Ordering::SeqCst),
        1,
        "same-key admissions share one scan"
    );
    assert_eq!(probes.load(Ordering::SeqCst), 2);
}
