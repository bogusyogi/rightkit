//! End to end: the real `rightkit-qa run-tests` binary compiles a tiny fixture crate (one passing and one
//! failing test among others) with cargo, runs its test executables outside any broker, and leaves a
//! receipt. Proves the exit codes, the receipt, libtest argument pass-through, the deadline kill and the
//! `[run_tests]` config knob against a real compile rather than a recorded listing.
//!
//! The fixture is written at run time into a temp directory (never into the repository, so no tool that
//! scans the tree sees a second Cargo workspace) and compiled with plain cargo (`--build-with cargo`): a
//! test run must not need, or queue on, the broker it is a tool for.
use rightkit_qa::run_tests::receipt::{Receipt, Status, RECEIPT_SCHEMA};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rightkit-qa");

const CARGO_TOML: &str = r#"[package]
name = "rkfixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

# Its own workspace root: the fixture is never a member of whatever directory tree it is made under.
[workspace]
"#;

const LIB_RS: &str = r#"pub fn two() -> u32 {
    2
}

#[cfg(test)]
mod unit {
    #[test]
    fn lib_unit_passes() {
        assert_eq!(super::two(), 2);
    }
}
"#;

const MIXED_RS: &str = r#"#[test]
fn passes() {
    assert_eq!(rkfixture::two(), 2);
}

#[test]
fn fails() {
    panic!("boom from the fixture");
}

#[test]
#[ignore]
fn ignored() {}
"#;

const ONLY_PASS_RS: &str = r#"#[test]
fn ok_one() {}

#[test]
fn ok_two() {
    assert_eq!(std::env::var("CARGO_PKG_NAME").unwrap(), "rkfixture");
    assert!(std::env::var("CARGO_MANIFEST_DIR").is_ok());
}
"#;

const SLOW_RS: &str = r#"#[test]
fn sleeps() {
    std::thread::sleep(std::time::Duration::from_secs(120));
}
"#;

/// The tests in this file. The last one to finish removes the fixture, its target directory included.
const TESTS: usize = 11;

struct Fixture {
    dir: PathBuf,
    root: PathBuf,
    target: PathBuf,
}

struct Done;

impl Drop for Done {
    fn drop(&mut self) {
        static FINISHED: AtomicUsize = AtomicUsize::new(0);
        if FINISHED.fetch_add(1, Ordering::SeqCst) + 1 == TESTS {
            let _ = std::fs::remove_dir_all(&fixture().dir);
        }
    }
}

fn fixture() -> &'static Fixture {
    static FX: OnceLock<Fixture> = OnceLock::new();
    FX.get_or_init(|| {
        // Cargo gives integration tests a scratch directory inside the target directory.
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("run-tests-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("app");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("Cargo.toml"), CARGO_TOML).unwrap();
        std::fs::write(root.join("src/lib.rs"), LIB_RS).unwrap();
        std::fs::write(root.join("tests/mixed.rs"), MIXED_RS).unwrap();
        std::fs::write(root.join("tests/only_pass.rs"), ONLY_PASS_RS).unwrap();
        std::fs::write(root.join("tests/slow.rs"), SLOW_RS).unwrap();
        Fixture {
            target: dir.join("target"),
            dir,
            root,
        }
    })
}

struct Ran {
    out: Output,
    receipt: Option<Receipt>,
    raw: Option<Value>,
    managed: PathBuf,
}

impl Ran {
    fn code(&self) -> i32 {
        self.out
            .status
            .code()
            .expect("run-tests was killed by a signal")
    }
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.out.stderr).into_owned()
    }
    fn receipt(&self) -> &Receipt {
        self.receipt.as_ref().unwrap_or_else(|| {
            panic!(
                "no receipt\nstdout:\n{}\nstderr:\n{}",
                self.stdout(),
                self.stderr()
            )
        })
    }
}

/// Run `rightkit-qa run-tests --root <fixture> --app rkfixture --build-with cargo ...` with a managed root
/// of its own (so parallel tests never prune each other's runs) and the receipt copied out.
fn run_tests(name: &str, extra: &[&str]) -> Ran {
    let fx = fixture();
    let managed = fx.dir.join(format!("managed-{name}"));
    let receipt_path = fx.dir.join(format!("receipt-{name}.json"));
    let _ = std::fs::remove_file(&receipt_path);
    let out = Command::new(BIN)
        .arg("run-tests")
        .args(["--root", fx.root.to_str().unwrap()])
        .args(["--managed-root", managed.to_str().unwrap()])
        .args(["--receipt", receipt_path.to_str().unwrap()])
        .args(extra)
        .env("CARGO_TARGET_DIR", &fx.target)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .expect("start rightkit-qa");
    let bytes = std::fs::read(&receipt_path).ok();
    Ran {
        receipt: bytes
            .as_ref()
            .map(|b| serde_json::from_slice(b).expect("receipt parses as a Receipt")),
        raw: bytes
            .as_ref()
            .map(|b| serde_json::from_slice(b).expect("receipt is JSON")),
        out,
        managed,
    }
}

const COMMON: [&str; 4] = ["--app", "rkfixture", "--build-with", "cargo"];

fn args<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    COMMON.iter().copied().chain(rest.iter().copied()).collect()
}

#[test]
fn a_passing_test_target_exits_zero_with_a_receipt_and_one_summary_line() {
    let _done = Done;
    let ran = run_tests("pass", &args(&["--offline", "--test", "only_pass"]));
    assert_eq!(
        ran.code(),
        0,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let r = ran.receipt();
    assert_eq!(r.schema, RECEIPT_SCHEMA);
    assert_eq!(r.app, "rkfixture");
    assert!(r.verdict.passed && r.verdict.exit_code == 0);
    assert_eq!(r.compile.source, "cargo");
    assert!(!r.compile.managed);
    assert_eq!(r.compile.exit, Some(0));
    assert_eq!(r.compile.build_finished, Some(true));
    assert_eq!(r.binaries.len(), 1);
    let b = &r.binaries[0];
    assert_eq!(
        (b.package.as_str(), b.target.as_str()),
        ("rkfixture", "only_pass")
    );
    assert_eq!(b.status, Status::Passed);
    assert_eq!(b.exit, Some(0));
    let s = b.summary.as_ref().expect("libtest summary parsed");
    assert_eq!((s.passed, s.failed, s.ignored), (2, 0, 0));
    assert!(Path::new(b.stdout.as_ref().unwrap()).is_file());
    assert!(std::fs::read_to_string(b.stdout.as_ref().unwrap())
        .unwrap()
        .contains("test result: ok. 2 passed"));
    // The tests ran from their package directory with the variables cargo gives a test process
    // (`ok_two` asserts them), and one summary line names the binary.
    assert!(Path::new(&b.cwd).ends_with("app"), "{}", b.cwd);
    assert!(
        ran.stdout()
            .lines()
            .any(|l| l
                .starts_with("run-tests: rkfixture::only_pass exit=0 passed=2 failed=0 ignored=0")),
        "{}",
        ran.stdout()
    );
    // The run is in the managed storage, beside its listing and logs.
    let runs = ran.managed.join("runs").join("rkfixture-run-tests");
    assert!(runs.is_dir(), "{}", runs.display());
    assert!(runs.join(&r.run_id).join("receipt.json").is_file());
    assert!(runs.join(&r.run_id).join("listing.jsonl").is_file());
}

#[test]
fn a_failing_test_fails_the_run_and_the_receipt_names_it() {
    let _done = Done;
    let ran = run_tests("fail", &args(&["--offline", "--test", "mixed"]));
    assert_eq!(
        ran.code(),
        1,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let r = ran.receipt();
    assert!(!r.verdict.passed);
    assert_eq!(r.verdict.exit_code, 1);
    assert_eq!(r.verdict.reasons, ["rkfixture::mixed failed (exit 101)"]);
    let b = &r.binaries[0];
    assert_eq!(b.status, Status::Failed);
    assert_eq!(b.exit, Some(101));
    assert_eq!(b.failed_tests, ["fails"]);
    let s = b.summary.as_ref().unwrap();
    assert_eq!((s.passed, s.failed, s.ignored), (1, 1, 1));
    assert!(!s.ok);
    assert!(std::fs::read_to_string(b.stdout.as_ref().unwrap())
        .unwrap()
        .contains("boom from the fixture"));
    assert!(
        ran.stdout()
            .contains("run-tests: rkfixture::mixed exit=101 passed=1 failed=1 ignored=1"),
        "{}",
        ran.stdout()
    );
    assert!(
        ran.stderr()
            .contains("run-tests: rkfixture::mixed: FAILED fails"),
        "{}",
        ran.stderr()
    );
    assert!(
        ran.stderr()
            .contains("run-tests: FAILED: rkfixture::mixed failed (exit 101)"),
        "{}",
        ran.stderr()
    );
    // A failed run is never pruned.
    let runs = ran.managed.join("runs").join("rkfixture-run-tests");
    assert!(runs.join(&r.run_id).is_dir());
    let marker: Value = serde_json::from_slice(
        &std::fs::read(runs.join(&r.run_id).join(".rightkit-run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["outcome"], "failed");
}

#[test]
fn libtest_arguments_after_the_double_dash_reach_the_binary() {
    let _done = Done;
    // --exact selects the one passing test: the failing one and the ignored one are filtered out.
    let ran = run_tests(
        "exact",
        &args(&["--offline", "--test", "mixed", "--", "passes", "--exact"]),
    );
    assert_eq!(
        ran.code(),
        0,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let b = &ran.receipt().binaries[0];
    assert_eq!(b.args, ["passes", "--exact"]);
    let s = b.summary.as_ref().unwrap();
    assert_eq!(
        (s.passed, s.failed, s.ignored, s.filtered_out),
        (1, 0, 0, 2)
    );

    // --skip drops the failing test; the ignored one is still reported as ignored.
    let ran = run_tests(
        "skip",
        &args(&["--offline", "--test", "mixed", "--", "--skip", "fails"]),
    );
    assert_eq!(
        ran.code(),
        0,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let s = ran.receipt().binaries[0].summary.clone().unwrap();
    assert_eq!((s.passed, s.failed, s.ignored), (1, 0, 1));

    // ... and `--deny-ignored` turns that ignored test into a failure: it did not run.
    let ran = run_tests(
        "deny",
        &args(&[
            "--offline",
            "--test",
            "mixed",
            "--deny-ignored",
            "--",
            "--skip",
            "fails",
        ]),
    );
    assert_eq!(ran.code(), 1);
    assert!(ran.receipt().verdict.reasons[0].contains("1 ignored test(s) did not run"));

    // A filter that matches nothing is not a pass.
    let ran = run_tests(
        "nothing",
        &args(&["--offline", "--test", "mixed", "--", "no_such_test_name"]),
    );
    assert_eq!(ran.code(), 1);
    assert!(ran.receipt().verdict.reasons[0].starts_with("no test ran"));
    let ran = run_tests(
        "nothing-ok",
        &args(&[
            "--offline",
            "--test",
            "mixed",
            "--allow-empty",
            "--",
            "no_such_test_name",
        ]),
    );
    assert_eq!(ran.code(), 0);
}

#[test]
fn a_binary_over_its_deadline_is_killed_and_reported_as_timed_out() {
    let _done = Done;
    let started = Instant::now();
    let ran = run_tests(
        "timeout",
        &args(&["--offline", "--test", "slow", "--binary-timeout-s", "2"]),
    );
    assert_eq!(
        ran.code(),
        1,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    assert!(
        started.elapsed() < Duration::from_secs(100),
        "the 120 s sleep was not cut: {:?}",
        started.elapsed()
    );
    let b = &ran.receipt().binaries[0];
    assert_eq!(b.status, Status::TimedOut);
    assert!(b.timed_out);
    assert_eq!(b.exit, None);
    assert_eq!(b.timeout_s, Some(2));
    assert!(
        ran.stdout()
            .contains("run-tests: rkfixture::slow exit=timeout"),
        "{}",
        ran.stdout()
    );
    assert!(ran.receipt().verdict.reasons[0].contains("timed out after 2s"));
}

#[test]
fn several_binaries_each_get_a_summary_line_and_the_failing_one_decides_the_exit() {
    let _done = Done;
    let ran = run_tests(
        "several",
        &args(&[
            "--offline",
            "--test",
            "only_pass",
            "--test",
            "mixed",
            "--parallel",
            "2",
        ]),
    );
    assert_eq!(
        ran.code(),
        1,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let r = ran.receipt();
    let order: Vec<&str> = r.binaries.iter().map(|b| b.target.as_str()).collect();
    assert_eq!(
        order,
        ["mixed", "only_pass"],
        "binaries are ordered by package and target"
    );
    assert_eq!(r.totals.binaries, 2);
    assert_eq!((r.totals.passed, r.totals.failed), (1, 1));
    assert_eq!(r.totals.tests.passed, 3);
    assert_eq!(r.totals.tests.failed, 1);
    let lines: Vec<String> = ran
        .stdout()
        .lines()
        .filter(|l| l.starts_with("run-tests: rkfixture::") && l.contains(" exit="))
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "one summary line per binary: {lines:?}");
    assert_eq!(r.settings.parallel, 2);
}

#[test]
fn a_failed_compile_exits_three_and_runs_nothing() {
    let _done = Done;
    let ran = run_tests("compile", &args(&["--offline", "--test", "no_such_target"]));
    assert_eq!(
        ran.code(),
        3,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let r = ran.receipt();
    assert_eq!(r.verdict.exit_code, 3);
    assert!(r.binaries.is_empty());
    assert_eq!(r.compile.exit, Some(101));
    assert!(
        r.verdict.reasons[0].contains("the compile failed (exit 101)"),
        "{:?}",
        r.verdict
    );
    assert!(
        ran.stderr().contains("no test target named"),
        "{}",
        ran.stderr()
    );
}

#[test]
fn a_listing_from_cargo_runs_without_a_compile() {
    let _done = Done;
    let fx = fixture();
    let listing = fx.dir.join("only_pass.jsonl");
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "test",
            "--offline",
            "--no-run",
            "--message-format=json",
            "--test",
            "only_pass",
        ])
        .current_dir(&fx.root)
        .env("CARGO_TARGET_DIR", &fx.target)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(&listing, &out.stdout).unwrap();
    let ran = run_tests(
        "listing",
        &[
            "--app",
            "rkfixture",
            "--from-cargo-json",
            listing.to_str().unwrap(),
        ],
    );
    assert_eq!(
        ran.code(),
        0,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout(),
        ran.stderr()
    );
    let r = ran.receipt();
    assert_eq!(r.compile.source, "listing");
    assert_eq!(r.compile.exit, None, "nothing was compiled");
    assert_eq!(r.binaries.len(), 1);

    // A listing with no test executable is a setup error (exit 2), never a pass.
    let none = fx.dir.join("none.jsonl");
    std::fs::write(&none, "{\"reason\":\"build-finished\",\"success\":true}\n").unwrap();
    let ran = run_tests(
        "listing-none",
        &[
            "--app",
            "rkfixture",
            "--from-cargo-json",
            none.to_str().unwrap(),
        ],
    );
    assert_eq!(ran.code(), 2);
    assert!(ran.receipt().verdict.reasons[0].contains("listed no test executable"));
}

#[test]
fn the_config_file_knob_sets_the_defaults_and_the_receipt_records_them() {
    let _done = Done;
    let fx = fixture();
    // A second app root, so the config does not leak into the other tests: same sources, own config.
    let root = fx.dir.join("app-with-config");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("Cargo.toml"), CARGO_TOML).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB_RS).unwrap();
    std::fs::write(root.join("tests/mixed.rs"), MIXED_RS).unwrap();
    std::fs::write(
        root.join("rightkit-qa.toml"),
        "app = \"cfgapp\"\n[run_tests]\nparallel = 1\nbinary_timeout_s = 77\ntotal_timeout_s = 900\ntest_args = [\"--skip\", \"fails\"]\ndeny_ignored = false\n[run_tests.env]\nRK_FIXTURE_FLAG = \"on\"\n",
    )
    .unwrap();
    let managed = fx.dir.join("managed-config");
    let receipt_path = fx.dir.join("receipt-config.json");
    let out = Command::new(BIN)
        .args(["run-tests", "--root", root.to_str().unwrap()])
        .args(["--managed-root", managed.to_str().unwrap()])
        .args(["--receipt", receipt_path.to_str().unwrap()])
        .args(["--build-with", "cargo", "--offline", "--test", "mixed"])
        .env("CARGO_TARGET_DIR", &fx.target)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let r: Receipt = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(r.app, "cfgapp", "the app key comes from the config file");
    assert_eq!(r.settings.parallel, 1);
    assert_eq!(r.settings.binary_timeout_s, 77);
    assert_eq!(r.settings.total_timeout_s, 900);
    assert_eq!(r.settings.test_args, ["--skip", "fails"]);
    assert_eq!(r.settings.env_keys, ["RK_FIXTURE_FLAG"]);
    assert_eq!(
        r.settings.config.as_deref(),
        Some(root.join("rightkit-qa.toml").to_str().unwrap())
    );
    assert_eq!(r.binaries[0].args, ["--skip", "fails"]);
    assert!(managed.join("runs").join("cfgapp-run-tests").is_dir());
}

#[test]
fn the_receipt_has_the_documented_shape() {
    let _done = Done;
    let ran = run_tests("shape", &args(&["--offline", "--test", "only_pass"]));
    assert_eq!(ran.code(), 0);
    let v = ran.raw.as_ref().unwrap();
    for key in [
        "schema",
        "run_id",
        "app",
        "started_at",
        "finished_at",
        "duration_ms",
        "root",
        "host",
        "compile",
        "settings",
        "binaries",
        "totals",
        "verdict",
    ] {
        assert!(v.get(key).is_some(), "receipt lacks {key}");
    }
    for key in [
        "source",
        "managed",
        "program",
        "argv",
        "exit",
        "timed_out",
        "duration_ms",
        "listing",
        "listing_sha256",
        "build_finished",
        "test_binaries",
    ] {
        assert!(v["compile"].get(key).is_some(), "compile lacks {key}");
    }
    let b = &v["binaries"][0];
    for key in [
        "index",
        "package",
        "target",
        "kind",
        "binary",
        "sha256",
        "cwd",
        "args",
        "status",
        "exit",
        "signal",
        "timed_out",
        "timeout_s",
        "timeout_kind",
        "started_after_ms",
        "duration_ms",
        "stdout",
        "stderr",
        "stdout_bytes",
        "stderr_bytes",
        "summary",
        "failed_tests",
        "survivors",
        "tmpdir",
        "error",
    ] {
        assert!(b.get(key).is_some(), "binary record lacks {key}");
    }
    assert_eq!(v["schema"], "rightkit.run-tests.receipt/1");
    assert_eq!(b["status"], "passed");
    assert_eq!(b["summary"]["passed"], 2);
    assert!(v["compile"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "--no-run"));
    assert!(v["compile"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "--message-format=json"));
}

#[test]
fn bad_usage_exits_two_with_a_message_and_help_prints_the_usage() {
    let _done = Done;
    let out = Command::new(BIN)
        .args(["run-tests", "--parallel", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--parallel"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(BIN)
        .args(["run-tests", "--help"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("usage: rightkit-qa run-tests"));
    // No app key anywhere: refused, not guessed.
    let fx = fixture();
    let out = Command::new(BIN)
        .args([
            "run-tests",
            "--root",
            fx.root.to_str().unwrap(),
            "--build-with",
            "cargo",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs an app key"));
}

/// A run-tests that dies (Ctrl-C, a kill) takes its test binary with it instead of leaving it running.
#[cfg(unix)]
#[test]
fn killing_run_tests_takes_the_test_binary_down_with_it() {
    let _done = Done;
    let fx = fixture();
    let dir = fx.dir.join("orphan");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("sleeper-test");
    let _ = std::fs::remove_file(&exe);
    std::os::unix::fs::symlink("/bin/sh", &exe).unwrap();
    let listing = dir.join("listing.jsonl");
    std::fs::write(
        &listing,
        format!(
            "{{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file://{d}#sleeper@0.1.0\",\"manifest_path\":\"{d}/Cargo.toml\",\"target\":{{\"kind\":[\"test\"],\"name\":\"t\"}},\"profile\":{{\"test\":true}},\"executable\":\"{e}\"}}\n{{\"reason\":\"build-finished\",\"success\":true}}\n",
            d = dir.display(),
            e = exe.display()
        ),
    )
    .unwrap();
    let pid_file = dir.join("pid");
    let mut run = Command::new(BIN)
        .args([
            "run-tests",
            "--app",
            "rkfixture",
            "--root",
            fx.root.to_str().unwrap(),
        ])
        .args(["--managed-root", dir.join("managed").to_str().unwrap()])
        .args(["--from-cargo-json", listing.to_str().unwrap()])
        .args([
            "--",
            "-c",
            &format!("echo $$ > '{}'; exec sleep 120", pid_file.display()),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the test binary never started"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let alive = |pid: u32| {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(alive(pid), "the test binary should be running");
    run.kill().unwrap();
    run.wait().unwrap();
    let started = Instant::now();
    while alive(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the test binary outlived run-tests"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
