//! Shared-fixture conformance for `rightkit-ownership`.
//!
//! Runs every case under `packages/release/fixtures/ownership/cases` in place (no vendored
//! copy), grouped by `kind`: verdict, cache-key, identity, admission, transport and scan.
//! Each group is one `#[test]` that runs all of its cases, reports `passed/total`, and
//! fails listing every failing case.
//!
//! Harness translations, from the Node harness in `fixtures/ownership/generate.mjs`:
//! - a Node `throw` from a runner becomes a `{"error": message}` reply;
//! - scripted scan replies are consumed in order, and an exhausted queue replies with
//!   `fixture: no scripted scan response`;
//! - `now()` is a `ManualClock` advanced by `advanceClockMs`;
//! - scan cases run the real scanner over a materialised git repository with the pinned
//!   clock, host string, scanner policy digest and git environment.
//!
//! DEFECT-1: `admission-cache-no-expiry-by-age` is checked against the Rust behaviour
//! (the cache key carries the UTC day), as CONTRACT.md requires.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_ownership::keyauth::AuthorityEnv;
use rightkit_ownership::scan::ScanContext;
use rightkit_ownership::worker::Scanner;
use rightkit_ownership::{
    canonical_repository_identity, ownership_cache_key, ownership_verdict, AdmissionConfig,
    FnScanRunner, ManualClock, Mode, OwnershipAdmission, ProcessScanRunner, ScanRequest,
    ScanRunner, Target, Verdict, WorkerLimits, DEFAULT_TIMEOUT,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

static NULL: Value = Value::Null;

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("release")
        .join("fixtures")
        .join("ownership")
        .join("cases")
}

fn load(name: &str) -> (Value, Value) {
    let dir = cases_dir().join(name);
    let read = |file: &str| -> Value {
        let path = dir.join(file);
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    (read("input.json"), read("expected.json"))
}

fn at<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    at(value, key).as_str().unwrap_or("")
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn verdict_json(decision: &rightkit_ownership::Decision) -> Value {
    json!({"admit": decision.admit, "code": decision.code, "problems": decision.problems})
}

/// Case names of one `kind`, sorted.
fn names_of(kind: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(cases_dir())
        .expect("cases dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            let input = cases_dir().join(name).join("input.json");
            fs::read_to_string(input)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|v| v["kind"] == kind)
        })
        .collect();
    names.sort();
    names
}

/// Runs each case, catching panics, and fails listing every failing case.
fn run_group(kind: &str, expected_total: usize, run: impl Fn(&str)) {
    let names = names_of(kind);
    assert_eq!(
        names.len(),
        expected_total,
        "{kind}: the case set changed; update the expected total"
    );
    let mut failed: Vec<String> = Vec::new();
    for name in &names {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(name)));
        if let Err(payload) = outcome {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            failed.push(format!(
                "{name}: {}",
                message.lines().take(8).collect::<Vec<_>>().join(" | ")
            ));
        }
    }
    println!(
        "fixtures {kind}: {}/{} passed",
        names.len() - failed.len(),
        names.len()
    );
    assert!(
        failed.is_empty(),
        "{kind}: {} failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

// ---- verdict -------------------------------------------------------------------------

fn verdict_case(name: &str) {
    let (input, expected) = load(name);
    let decision = ownership_verdict(at(&input, "report"), at(&input, "trust"));
    assert_eq!(
        json!({"verdict": verdict_json(&decision)}),
        expected,
        "fixture {name}"
    );
}

#[test]
fn fixtures_verdict() {
    run_group("verdict", 88, verdict_case);
}

// ---- cache key -----------------------------------------------------------------------

fn cache_key_case(name: &str) {
    let (input, expected) = load(name);
    let actual = match ownership_cache_key(at(&input, "inputs")) {
        Ok(key) => json!({"key": key}),
        Err(error) => json!({"error": {"code": error.code, "message": error.message}}),
    };
    assert_eq!(actual, expected, "fixture {name}");
}

#[test]
fn fixtures_cache_key() {
    run_group("cache-key", 24, cache_key_case);
}

// ---- identity ------------------------------------------------------------------------

fn identity_case(name: &str) {
    let (input, expected) = load(name);
    let actual = match canonical_repository_identity(at(&input, "value")) {
        Ok(canonical) => json!({"canonical": canonical}),
        Err(message) => json!({"error": message}),
    };
    assert_eq!(actual, expected, "fixture {name}");
}

#[test]
fn fixtures_identity() {
    run_group("identity", 71, identity_case);
}

// ---- transport -----------------------------------------------------------------------

fn scrub(value: Value, paths: &[String], placeholder: &str) -> Value {
    match value {
        Value::String(text) => {
            let mut text = text;
            for path in paths {
                text = text.replace(path.as_str(), placeholder);
            }
            Value::String(text)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| scrub(item, paths, placeholder))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, scrub(v, paths, placeholder)))
                .collect(),
        ),
        other => other,
    }
}

fn transport_case(name: &str) {
    let (input, mut expected) = load(name);
    let base = tempfile::tempdir().expect("tempdir");
    let canonical = fs::canonicalize(base.path()).expect("canonical tempdir");
    fs::write(
        canonical.join("ownership-scan-worker.mjs"),
        text(&input, "workerSource"),
    )
    .expect("write worker");
    let exec = text(&input, "execPath");
    let runner = ProcessScanRunner::new(if exec == "node" { "node" } else { exec });
    let timeout = Duration::from_millis(at(&input, "timeoutMs").as_u64().expect("timeoutMs"));
    let raw = runner.run(&ScanRequest {
        scanner_root: &canonical,
        scan_root: "/fixture/repo",
        timeout,
        inputs_only: at(&input, "inputsOnly").as_bool().unwrap_or(false),
    });
    let mut paths = vec![
        canonical.to_string_lossy().into_owned(),
        base.path().to_string_lossy().into_owned(),
    ];
    paths.dedup();
    let mut result = scrub(raw, &paths, "<SCANNER>");
    if let Some(argv) = result.get_mut("argv").and_then(Value::as_array_mut) {
        for arg in argv {
            if let Some(text) = arg.as_str().filter(|t| t.starts_with("<SCANNER>")) {
                *arg = json!(text.replace('\\', "/"));
            }
        }
    }
    if let Some(error) = result
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        if error.starts_with("scanner returned invalid JSON: ") {
            result["error"] = json!("scanner returned invalid JSON: <parser message>");
        }
    }
    const NOT_FOUND: &str = "scanner failed to start: <spawn error: not found>";
    if result
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|e| {
            e.starts_with("scanner failed to start: ")
                && [
                    "No such file or directory",
                    "cannot find the path specified",
                    "cannot find the file specified",
                ]
                .iter()
                .any(|t| e.contains(t))
        })
    {
        result["error"] = json!(NOT_FOUND);
    }
    if expected["result"]["error"]
        .as_str()
        .is_some_and(|e| e.starts_with("scanner failed to start: spawn ") && e.ends_with("ENOENT"))
    {
        expected["result"]["error"] = json!(NOT_FOUND);
    }
    assert_eq!(json!({"result": result}), expected, "fixture {name}");
}

#[test]
fn fixtures_transport() {
    run_group("transport", 10, transport_case);
}

// ---- admission -----------------------------------------------------------------------

#[derive(Default)]
struct Cursor {
    observed: Option<Value>,
    throws: Option<String>,
}

#[derive(Default)]
struct Shared {
    cursor: Mutex<Cursor>,
    last: Mutex<Option<Value>>,
    inputs: AtomicUsize,
    scans: AtomicUsize,
    scan_replies: Mutex<VecDeque<Value>>,
}

fn wait_for(condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "harness wait timed out"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn inputs_reply(shared: &Shared) -> Value {
    shared.inputs.fetch_add(1, Ordering::SeqCst);
    let cursor = shared.cursor.lock().expect("cursor");
    if let Some(message) = &cursor.throws {
        return json!({"error": message});
    }
    if let Some(observed) = &cursor.observed {
        *shared.last.lock().expect("last") = Some(observed.clone());
    }
    drop(cursor);
    let last = shared.last.lock().expect("last").clone();
    last.unwrap_or_else(|| json!({"error": "fixture: no scripted inputs response"}))
}

fn scan_reply(shared: &Shared, concurrent: bool) -> Value {
    shared.scans.fetch_add(1, Ordering::SeqCst);
    if concurrent {
        // Hold the leader's scan open until the follower's probe has run, so the follower
        // claims the same in-flight key rather than a finished cache entry.
        wait_for(|| shared.inputs.load(Ordering::SeqCst) >= 2);
        thread::sleep(Duration::from_millis(100));
    }
    let reply = shared
        .scan_replies
        .lock()
        .expect("scan replies")
        .pop_front();
    reply.unwrap_or_else(|| json!({"error": "fixture: no scripted scan response"}))
}

fn violation_json(v: &rightkit_ownership::Violation) -> Value {
    json!({"capability": v.capability, "owner": v.owner, "fingerprint": v.fingerprint, "file": v.file, "line": v.line})
}

fn project(v: &Verdict) -> Value {
    if v.outcome == "off" {
        return json!({"mode": v.mode.as_str(), "outcome": "off"});
    }
    let mut out = json!({
        "mode": v.mode.as_str(),
        "admit": v.admit,
        "code": v.code,
        "problems": v.problems,
        "outcome": v.outcome,
        "violations": v.violations.iter().map(violation_json).collect::<Vec<_>>(),
        "scanRoot": v.scan_root,
        "scanMs": v.scan_ms,
        "cached": v.cached,
    });
    if let Some(error) = &v.error {
        out["error"] = json!(error);
    }
    out
}

fn assert_json(gate: &OwnershipAdmission, verdict: &Verdict) -> Value {
    match gate.assert_admitted(verdict) {
        Ok(()) => json!({"admitted": true}),
        Err(refusal) => {
            json!({"admitted": false, "code": refusal.code, "message": refusal.message})
        }
    }
}

fn verdict_from_json(gate: &OwnershipAdmission, item: &Value) -> Verdict {
    Verdict {
        mode: gate.mode(),
        outcome: "",
        admit: item.get("admit").and_then(Value::as_bool).unwrap_or(false),
        code: opt_text(item, "code"),
        scan_root: None,
        error: opt_text(item, "error"),
        problems: item
            .get("problems")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        violations: Vec::new(),
        scan_ms: 0,
        cached: false,
    }
}

fn run_admission(scenario: &Value, extra_scans: &[Value]) -> Value {
    let concurrent = scenario
        .get("concurrent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let steps: Vec<Value> = at(scenario, "steps")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut replies: VecDeque<Value> = steps
        .iter()
        .filter_map(|step| step.get("scanned").cloned())
        .collect();
    replies.extend(extra_scans.iter().cloned());
    let shared = Arc::new(Shared {
        scan_replies: Mutex::new(replies),
        ..Shared::default()
    });
    let clock = ManualClock::new(0);
    let mode = Mode::parse(text(scenario, "mode")).expect("fixture mode");
    let scanner_root = at(scenario, "scannerRoot").as_str().map(PathBuf::from);
    let (inputs_shared, scan_shared) = (Arc::clone(&shared), Arc::clone(&shared));
    let gate = Arc::new(OwnershipAdmission::new(AdmissionConfig {
        mode,
        timeout: DEFAULT_TIMEOUT,
        scanner_root,
        clock: Arc::new(clock.clone()),
        inputs_runner: Arc::new(FnScanRunner(move |_: &ScanRequest<'_>| {
            inputs_reply(&inputs_shared)
        })),
        scan_runner: Arc::new(FnScanRunner(move |_: &ScanRequest<'_>| {
            scan_reply(&scan_shared, concurrent)
        })),
        limits: WorkerLimits::default(),
    }));
    let set_cursor = |step: &Value| {
        *shared.cursor.lock().expect("cursor") = Cursor {
            observed: step.get("observed").cloned(),
            throws: opt_text(step, "observedThrows"),
        };
    };
    let mut verdicts = Vec::new();
    if concurrent {
        let mut handles = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            set_cursor(step);
            let (root, workspace) = (
                opt_text(step, "repositoryRoot"),
                opt_text(step, "workspaceRoot"),
            );
            let gate = Arc::clone(&gate);
            handles.push(thread::spawn(move || {
                gate.evaluate(Target {
                    repository_root: root.as_deref(),
                    workspace_root: workspace.as_deref(),
                })
            }));
            let shared_probe = Arc::clone(&shared);
            wait_for(move || shared_probe.inputs.load(Ordering::SeqCst) > index);
        }
        for handle in handles {
            verdicts.push(handle.join().expect("evaluation thread"));
        }
    } else {
        for step in &steps {
            if let Some(ms) = step.get("advanceClockMs").and_then(Value::as_u64) {
                clock.advance(ms);
            }
            set_cursor(step);
            let (root, workspace) = (
                opt_text(step, "repositoryRoot"),
                opt_text(step, "workspaceRoot"),
            );
            verdicts.push(gate.evaluate(Target {
                repository_root: root.as_deref(),
                workspace_root: workspace.as_deref(),
            }));
        }
    }
    let mut out = json!({
        "steps": verdicts.iter().map(|v| json!({"result": project(v), "assertAdmitted": assert_json(&gate, v)})).collect::<Vec<_>>(),
        "calls": {"inputs": shared.inputs.load(Ordering::SeqCst), "scan": shared.scans.load(Ordering::SeqCst)},
    });
    if let Some(items) = scenario.get("assertOnly").and_then(Value::as_array) {
        out["assertOnly"] = items.iter().map(|item| json!({"input": item, "assertAdmitted": assert_json(&gate, &verdict_from_json(&gate, item))})).collect::<Value>();
    }
    out
}

fn admission_case(name: &str) {
    let (input, expected) = load(name);
    if name == "admission-cache-no-expiry-by-age" {
        return cache_no_expiry_by_age_rust_behaviour(&input, &expected);
    }
    assert_eq!(
        run_admission(at(&input, "scenario"), &[]),
        expected,
        "fixture {name}"
    );
}

/// DEFECT-1 (CONTRACT.md section 4). Node serves step 2 from cache and never rescans. The
/// Rust crate keys the cache by UTC date, so step 2 (a year later) must rescan. The
/// fixture scripts no second scan reply, so one is scripted: step 1's own clean reply.
fn cache_no_expiry_by_age_rust_behaviour(input: &Value, expected: &Value) {
    let scenario = at(input, "scenario");
    let rescan_reply = at(&at(scenario, "steps")[0], "scanned").clone();
    let actual = run_admission(scenario, &[rescan_reply]);
    let rust_required = at(scenario, "rustRequired");
    assert_eq!(rust_required["calls.scan"], json!(2));
    assert_eq!(rust_required["steps[1].result.cached"], json!(false));
    assert_eq!(actual["calls"]["scan"], json!(2), "step 2 must rescan");
    assert_eq!(
        actual["steps"][0], expected["steps"][0],
        "step 1 is the fixture's clean scan"
    );
    let mut expected_step2 = expected["steps"][1].clone();
    expected_step2["result"]["cached"] = json!(false);
    assert_eq!(
        actual["steps"][1]["result"], expected_step2["result"],
        "rescanned verdict"
    );
    assert_eq!(
        actual["steps"][1]["assertAdmitted"],
        expected_step2["assertAdmitted"]
    );
    assert_eq!(actual["calls"]["inputs"], expected["calls"]["inputs"]);
}

#[test]
fn fixtures_admission() {
    run_group("admission", 62, admission_case);
}

// ---- scan ----------------------------------------------------------------------------

const FIXED_HOST: &str = "fixture-host/fixture-os";

fn git_env_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    command
}

fn git(root: &Path, args: &[&str]) {
    let output = git_env_command(root).args(args).output().expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn put(root: &Path, rel: &str, content: &str) {
    let mut file = root.to_path_buf();
    for part in rel.split('/') {
        file.push(part);
    }
    fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    fs::write(file, content).expect("write");
}

fn materialize(input: &Value, base: &Path) -> PathBuf {
    let root = base.join("repo");
    fs::create_dir_all(&root).expect("repo dir");
    git(&root, &["init", "-q", "-b", "main"]);
    if let Some(origin) = at(at(input, "harness"), "origin").as_str() {
        git(&root, &["remote", "add", "origin", origin]);
    }
    let repo = at(input, "repo");
    for (rel, content) in at(repo, "files").as_object().into_iter().flatten() {
        put(&root, rel, content.as_str().unwrap_or(""));
    }
    for (rel, target) in at(repo, "symlinks").as_object().into_iter().flatten() {
        let mut link = root.clone();
        for part in rel.split('/') {
            link.push(part);
        }
        fs::create_dir_all(link.parent().expect("parent")).expect("mkdir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(target.as_str().unwrap_or(""), &link).expect("symlink");
        #[cfg(not(unix))]
        let _ = (target, link);
    }
    if at(at(input, "harness"), "commit").as_bool() != Some(false) {
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
    }
    for (rel, content) in at(repo, "untracked").as_object().into_iter().flatten() {
        put(&root, rel, content.as_str().unwrap_or(""));
    }
    root
}

/// Unix milliseconds of an ISO-8601 UTC timestamp.
fn ms_of(iso: &str) -> u64 {
    let (date, time) = iso.trim_end_matches('Z').split_once('T').expect("iso");
    let d: Vec<i64> = date
        .split('-')
        .map(|p| p.parse().expect("date part"))
        .collect();
    let (y, m, day) = (if d[1] <= 2 { d[0] - 1 } else { d[0] }, d[1], d[2]);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let (hms, millis) = time.split_once('.').unwrap_or((time, "0"));
    let t: Vec<i64> = hms
        .split(':')
        .map(|p| p.parse().expect("time part"))
        .collect();
    ((days * 86_400 + t[0] * 3600 + t[1] * 60 + t[2]) * 1000
        + millis.parse::<i64>().expect("millis")) as u64
}

fn scanner_for(input: &Value, mutate: bool, mutated: Arc<AtomicUsize>) -> Scanner {
    let state = at(input, "trustState").clone();
    let hook: Option<rightkit_ownership::scan::InventoryHook> = if mutate {
        Some(Arc::new(move |root: &Path, call: usize| {
            if call == 2 {
                mutated.fetch_add(1, Ordering::SeqCst);
                fs::write(root.join("fixture-mutation.txt"), "changed\n").expect("mutate");
            }
        }))
    } else {
        None
    };
    let ctx = ScanContext {
        clock: Arc::new(ManualClock::new(ms_of(text(at(input, "harness"), "now")))),
        host: FIXED_HOST.into(),
        authority: AuthorityEnv::unavailable(),
        trust_loader: Arc::new(move || Ok(state.clone())),
        policy_digest: hex::encode(Sha256::digest(
            b"rightkit-ownership-fixture-scanner-policy-v1",
        )),
        before_inventory: hook,
    };
    Scanner::new(ctx, at(input, "catalog").clone())
}

fn normalize_parser_text(value: Value) -> Value {
    match value {
        Value::String(text) => match text.find("Invalid TOML document: ") {
            Some(at) => Value::String(format!(
                "{}Invalid TOML document: <parser message>",
                &text[..at]
            )),
            None => Value::String(text),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_parser_text).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, normalize_parser_text(v)))
                .collect(),
        ),
        other => other,
    }
}

fn scan_case(name: &str) {
    let (input, expected) = load(name);
    let base_dir = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(base_dir.path()).expect("canonical");
    let mutate = at(at(&input, "harness"), "mutateOnSecondInventory").as_bool() == Some(true);
    let scan_root = materialize(&input, &base.join("scan"));
    let inputs_root = materialize(&input, &base.join("inputs"));
    let scan = scanner_for(&input, mutate, Arc::new(AtomicUsize::new(0))).scan(&scan_root);
    let inputs_mode =
        scanner_for(&input, false, Arc::new(AtomicUsize::new(0))).inputs(&inputs_root);
    let replacements = vec![
        scan_root.to_string_lossy().into_owned(),
        inputs_root.to_string_lossy().into_owned(),
    ];
    let scrub_all = |value: Value| {
        let value = scrub(value, &replacements, "<ROOT>");
        scrub(value, &[base.to_string_lossy().into_owned()], "<TMP>")
    };
    let cache_key = scan
        .get("inputs")
        .map(|inputs| ownership_cache_key(inputs).expect("cache key"));
    let actual = json!({"scan": scrub_all(scan), "inputsMode": scrub_all(inputs_mode), "cacheKey": cache_key});
    // TOML parser error text is engine specific (smol-toml vs the `toml` crate) and not
    // contractual: keep only the prefix, as the transport group does for JSON parsers.
    assert_eq!(
        normalize_parser_text(actual),
        normalize_parser_text(expected),
        "fixture {name}"
    );
}

#[test]
fn fixtures_scan() {
    run_group("scan", 38, scan_case);
}
