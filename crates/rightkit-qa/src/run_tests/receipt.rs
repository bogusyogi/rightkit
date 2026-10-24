//! The run-tests receipt: one JSON file, `rightkit.run-tests.receipt/1`, written under the run's own
//! directory in the managed run storage, whatever the outcome. It names the compile, every binary that
//! was (or was not) run, what each said, and the verdict the process exit code comes from.
//!
//! The verdict is computed here from the facts in the receipt ([`judge`]), never passed in, so the file
//! and the exit code cannot disagree.
use super::artifacts::TestBinary;
use super::libtest::LibtestSummary;
use crate::config::RunTestsConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const RECEIPT_SCHEMA: &str = "rightkit.run-tests.receipt/1";
pub const RECEIPT_FILE: &str = "receipt.json";

/// Every test binary passed (and the run's own rules held).
pub const EXIT_PASSED: i32 = 0;
/// A binary failed, timed out or could not be started, or a run rule (ignored tests, no test ran) failed.
pub const EXIT_FAILED: i32 = 1;
/// The run could not be set up: bad arguments, no test binary listed, an unreadable listing.
pub const EXIT_SETUP: i32 = 2;
/// The compile failed or timed out. No test ran.
pub const EXIT_COMPILE: i32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub schema: String,
    pub run_id: String,
    /// The app key the user gave. Run storage uses `<app>-run-tests` so retention of these runs never
    /// competes with the app's journeys runs.
    pub app: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    /// The app root the command ran in.
    pub root: String,
    pub host: HostInfo,
    pub compile: CompileRecord,
    pub settings: SettingsRecord,
    pub binaries: Vec<BinaryRecord>,
    pub totals: Totals,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
}

impl HostInfo {
    pub fn current() -> Self {
        HostInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

/// How the test executables were obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileRecord {
    /// `broker` (`rustright cargo ...`), `cargo` (plain cargo) or `listing` (`--from-cargo-json`).
    pub source: String,
    /// True when the compile went through the managed client.
    pub managed: bool,
    pub program: Option<String>,
    pub argv: Vec<String>,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: Option<u64>,
    /// The broker request id, read from the client's stderr line when it printed one.
    pub request_id: Option<String>,
    /// Why this compile route was chosen when it is not the managed one.
    pub note: Option<String>,
    /// The cargo JSON listing kept in the run directory.
    pub listing: Option<String>,
    pub listing_sha256: Option<String>,
    /// Cargo's own `build-finished` verdict, when the listing has one.
    pub build_finished: Option<bool>,
    /// Test executables the listing named.
    pub test_binaries: usize,
}

impl CompileRecord {
    pub fn empty(source: &str) -> Self {
        CompileRecord {
            source: source.to_string(),
            managed: false,
            program: None,
            argv: vec![],
            exit: None,
            timed_out: false,
            duration_ms: None,
            request_id: None,
            note: None,
            listing: None,
            listing_sha256: None,
            build_finished: None,
            test_binaries: 0,
        }
    }
}

/// The settings the run used after the command line, the config file and the defaults were merged.
/// Environment variable NAMES are recorded, never values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsRecord {
    pub parallel: usize,
    pub binary_timeout_s: u64,
    pub total_timeout_s: u64,
    pub compile_timeout_s: u64,
    pub test_args: Vec<String>,
    pub cwd: String,
    pub env_keys: Vec<String>,
    pub unset_env: Vec<String>,
    pub isolate_tmp: bool,
    pub deny_ignored: bool,
    pub allow_empty: bool,
    pub require_broker: bool,
    /// Where the defaults came from: the config file, or none.
    pub config: Option<String>,
}

impl SettingsRecord {
    /// The built-in defaults, as a record (tests and the no-config case).
    pub fn defaults() -> Self {
        SettingsRecord {
            parallel: RunTestsConfig::DEFAULT_PARALLEL,
            binary_timeout_s: RunTestsConfig::DEFAULT_BINARY_TIMEOUT_S,
            total_timeout_s: RunTestsConfig::DEFAULT_TOTAL_TIMEOUT_S,
            compile_timeout_s: RunTestsConfig::DEFAULT_COMPILE_TIMEOUT_S,
            test_args: vec![],
            cwd: "package".to_string(),
            env_keys: vec![],
            unset_env: RunTestsConfig::DEFAULT_UNSET_ENV
                .iter()
                .map(|s| s.to_string())
                .collect(),
            isolate_tmp: false,
            deny_ignored: false,
            allow_empty: false,
            require_broker: false,
            config: None,
        }
    }
}

/// How one binary ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Passed,
    Failed,
    TimedOut,
    /// The binary could not be started (missing file, bad working directory).
    Error,
    /// The total deadline passed before this binary started.
    NotRun,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::TimedOut => "timed-out",
            Status::Error => "error",
            Status::NotRun => "not-run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurvivorRecord {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryRecord {
    pub index: usize,
    pub package: String,
    pub target: String,
    pub kind: Vec<String>,
    pub binary: String,
    pub sha256: Option<String>,
    pub cwd: String,
    pub args: Vec<String>,
    pub status: Status,
    /// The exit code. `None` when the binary was killed, ended by a signal, or never started.
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    /// The deadline that applied, in seconds, and which one it was (`binary` or `total`).
    pub timeout_s: Option<u64>,
    pub timeout_kind: Option<String>,
    /// Milliseconds from the start of the execution phase to this binary's start.
    pub started_after_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdout_bytes: Option<u64>,
    pub stderr_bytes: Option<u64>,
    /// The `test result:` counts, when the binary printed any.
    pub summary: Option<LibtestSummary>,
    pub failed_tests: Vec<String>,
    pub survivors: Vec<SurvivorRecord>,
    pub tmpdir: Option<String>,
    pub error: Option<String>,
}

impl BinaryRecord {
    /// A record for a binary that has not run (yet). Callers fill in what happened.
    pub fn pending(index: usize, bin: &TestBinary, args: &[String], cwd: &Path) -> Self {
        BinaryRecord {
            index,
            package: bin.package.clone(),
            target: bin.target.clone(),
            kind: bin.kind.clone(),
            binary: bin.executable.display().to_string(),
            sha256: None,
            cwd: cwd.display().to_string(),
            args: args.to_vec(),
            status: Status::NotRun,
            exit: None,
            signal: None,
            timed_out: false,
            timeout_s: None,
            timeout_kind: None,
            started_after_ms: None,
            duration_ms: None,
            stdout: None,
            stderr: None,
            stdout_bytes: None,
            stderr_bytes: None,
            summary: None,
            failed_tests: vec![],
            survivors: vec![],
            tmpdir: None,
            error: None,
        }
    }

    pub fn label(&self) -> String {
        format!("{}::{}", self.package, self.target)
    }

    pub fn passed(&self) -> bool {
        self.status == Status::Passed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered_out: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub binaries: usize,
    pub passed: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub errors: usize,
    pub not_run: usize,
    /// Binaries that printed no `test result:` line (a custom harness): their tests are not counted.
    pub without_summary: usize,
    pub tests: TestCounts,
}

impl Totals {
    pub fn of(binaries: &[BinaryRecord]) -> Self {
        let mut t = Totals {
            binaries: binaries.len(),
            ..Totals::default()
        };
        for b in binaries {
            match b.status {
                Status::Passed => t.passed += 1,
                Status::Failed => t.failed += 1,
                Status::TimedOut => t.timed_out += 1,
                Status::Error => t.errors += 1,
                Status::NotRun => t.not_run += 1,
            }
            match &b.summary {
                Some(s) => {
                    t.tests.passed += s.passed;
                    t.tests.failed += s.failed;
                    t.tests.ignored += s.ignored;
                    t.tests.measured += s.measured;
                    t.tests.filtered_out += s.filtered_out;
                }
                None if b.status != Status::NotRun && b.status != Status::Error => {
                    t.without_summary += 1
                }
                None => {}
            }
        }
        t
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub passed: bool,
    pub exit_code: i32,
    /// One line per reason the run did not pass. Empty on a pass.
    pub reasons: Vec<String>,
}

impl Verdict {
    pub fn pass() -> Self {
        Verdict {
            passed: true,
            exit_code: EXIT_PASSED,
            reasons: vec![],
        }
    }

    pub fn fail(exit_code: i32, reason: impl Into<String>) -> Self {
        Verdict {
            passed: false,
            exit_code,
            reasons: vec![reason.into()],
        }
    }
}

/// The run rules that apply after the binaries ran.
#[derive(Debug, Clone, Copy)]
pub struct Rules {
    pub deny_ignored: bool,
    pub allow_empty: bool,
}

/// The verdict of a run whose compile succeeded and whose binaries have run: a non-zero exit for any
/// binary that did not pass, for ignored tests when `deny_ignored`, and for a run in which every binary
/// reported a summary and not one test ran (a filter that matches nothing is not a pass).
pub fn judge(binaries: &[BinaryRecord], rules: Rules) -> Verdict {
    let mut reasons = vec![];
    for b in binaries {
        match b.status {
            Status::Passed => {}
            Status::Failed => reasons.push(match (b.exit, b.signal) {
                (Some(code), _) => format!("{} failed (exit {code})", b.label()),
                (None, Some(sig)) => format!("{} failed (signal {sig})", b.label()),
                (None, None) => format!("{} failed", b.label()),
            }),
            Status::TimedOut => reasons.push(format!(
                "{} timed out after {}s ({} deadline)",
                b.label(),
                b.timeout_s.unwrap_or(0),
                b.timeout_kind.as_deref().unwrap_or("binary")
            )),
            Status::Error => reasons.push(format!(
                "{} could not be run: {}",
                b.label(),
                b.error.as_deref().unwrap_or("unknown error")
            )),
            Status::NotRun => reasons.push(format!(
                "{} was not run: {}",
                b.label(),
                b.error
                    .as_deref()
                    .unwrap_or("the total deadline passed before it started")
            )),
        }
    }
    let totals = Totals::of(binaries);
    if rules.deny_ignored && totals.tests.ignored > 0 {
        reasons.push(format!(
            "{} ignored test(s) did not run (deny_ignored is set); an ignored test is a test that was not run",
            totals.tests.ignored
        ));
    }
    let all_counted = !binaries.is_empty() && binaries.iter().all(|b| b.summary.is_some());
    if !rules.allow_empty
        && all_counted
        && totals.tests.passed + totals.tests.failed == 0
        && reasons.is_empty()
    {
        reasons.push(
            "no test ran: every binary reported 0 passed and 0 failed (a filter that matches nothing is not a pass; --allow-empty accepts it)".to_string(),
        );
    }
    if reasons.is_empty() {
        Verdict::pass()
    } else {
        Verdict {
            passed: false,
            exit_code: EXIT_FAILED,
            reasons,
        }
    }
}

/// Write `receipt` to `dir/receipt.json` (a temp file renamed into place) and return the path.
pub fn write(dir: &Path, receipt: &Receipt) -> crate::util::Result<PathBuf> {
    let target = dir.join(RECEIPT_FILE);
    let temp = dir.join(format!(".{RECEIPT_FILE}.{}.tmp", std::process::id()));
    let mut text = serde_json::to_string_pretty(receipt)?;
    text.push('\n');
    std::fs::write(&temp, text)
        .map_err(|e| crate::util::Error(format!("cannot write {}: {e}", temp.display())))?;
    std::fs::rename(&temp, &target)
        .map_err(|e| crate::util::Error(format!("cannot place {}: {e}", target.display())))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(target: &str) -> TestBinary {
        TestBinary {
            package: "fx".into(),
            target: target.into(),
            kind: vec!["test".into()],
            executable: PathBuf::from(format!("/t/{target}")),
            manifest_dir: PathBuf::from("/w/fx"),
        }
    }

    fn rec(target: &str, status: Status, summary: Option<(u64, u64, u64)>) -> BinaryRecord {
        let mut r = BinaryRecord::pending(0, &bin(target), &[], Path::new("/w/fx"));
        r.status = status;
        r.exit = match status {
            Status::Passed => Some(0),
            Status::Failed => Some(101),
            _ => None,
        };
        r.timed_out = status == Status::TimedOut;
        r.timeout_s = Some(30);
        r.timeout_kind = Some("binary".into());
        r.summary = summary.map(|(passed, failed, ignored)| LibtestSummary {
            results: 1,
            ok: failed == 0,
            passed,
            failed,
            ignored,
            measured: 0,
            filtered_out: 0,
        });
        r
    }

    const RULES: Rules = Rules {
        deny_ignored: false,
        allow_empty: false,
    };

    #[test]
    fn all_passed_is_exit_zero() {
        let v = judge(
            &[
                rec("a", Status::Passed, Some((3, 0, 0))),
                rec("b", Status::Passed, Some((1, 0, 2))),
            ],
            RULES,
        );
        assert_eq!(v, Verdict::pass());
    }

    #[test]
    fn one_failed_binary_makes_the_run_fail_with_its_exit_code_named() {
        let v = judge(
            &[
                rec("a", Status::Passed, Some((3, 0, 0))),
                rec("b", Status::Failed, Some((0, 1, 0))),
            ],
            RULES,
        );
        assert!(!v.passed);
        assert_eq!(v.exit_code, EXIT_FAILED);
        assert_eq!(v.reasons, ["fx::b failed (exit 101)"]);
    }

    #[test]
    fn a_timeout_an_error_and_a_not_run_binary_each_fail_the_run() {
        let mut not_run = rec("c", Status::NotRun, None);
        not_run.error = Some("the total deadline passed before it started".into());
        let mut error = rec("b", Status::Error, None);
        error.error = Some("failed to start /t/b".into());
        let v = judge(&[rec("a", Status::TimedOut, None), error, not_run], RULES);
        assert_eq!(v.exit_code, EXIT_FAILED);
        assert_eq!(v.reasons.len(), 3);
        assert!(v.reasons[0].contains("timed out after 30s (binary deadline)"));
        assert!(v.reasons[1].contains("could not be run: failed to start"));
        assert!(v.reasons[2].contains("was not run"));
    }

    #[test]
    fn ignored_tests_fail_the_run_only_when_denied() {
        let bins = [rec("a", Status::Passed, Some((3, 0, 2)))];
        assert!(judge(&bins, RULES).passed);
        let v = judge(
            &bins,
            Rules {
                deny_ignored: true,
                allow_empty: false,
            },
        );
        assert!(!v.passed);
        assert!(v.reasons[0].contains("2 ignored test(s) did not run"));
    }

    #[test]
    fn a_run_in_which_no_test_ran_is_not_a_pass_unless_allowed() {
        let bins = [
            rec("a", Status::Passed, Some((0, 0, 0))),
            rec("b", Status::Passed, Some((0, 0, 3))),
        ];
        let v = judge(&bins, RULES);
        assert!(!v.passed, "{v:?}");
        assert!(v.reasons[0].starts_with("no test ran"));
        let allowed = judge(
            &bins,
            Rules {
                deny_ignored: false,
                allow_empty: true,
            },
        );
        assert!(allowed.passed);
    }

    #[test]
    fn a_binary_without_a_summary_makes_the_zero_count_unknown_not_empty() {
        // A custom harness prints no `test result:` line: its count is unknown, so the empty rule is silent.
        let bins = [
            rec("a", Status::Passed, Some((0, 0, 0))),
            rec("custom", Status::Passed, None),
        ];
        assert!(judge(&bins, RULES).passed);
        assert_eq!(Totals::of(&bins).without_summary, 1);
    }

    #[test]
    fn totals_count_statuses_and_sum_the_tests() {
        let bins = [
            rec("a", Status::Passed, Some((3, 0, 1))),
            rec("b", Status::Failed, Some((1, 2, 0))),
            rec("c", Status::TimedOut, None),
            rec("d", Status::NotRun, None),
        ];
        let t = Totals::of(&bins);
        assert_eq!(
            (
                t.binaries,
                t.passed,
                t.failed,
                t.timed_out,
                t.not_run,
                t.errors
            ),
            (4, 1, 1, 1, 1, 0)
        );
        assert_eq!(
            t.tests,
            TestCounts {
                passed: 4,
                failed: 2,
                ignored: 1,
                measured: 0,
                filtered_out: 0
            }
        );
        // The timed-out binary printed no summary; the never-started one is not "without summary".
        assert_eq!(t.without_summary, 1);
    }

    #[test]
    fn the_receipt_round_trips_and_has_the_documented_keys() {
        let binaries = vec![rec("a", Status::Passed, Some((3, 0, 0)))];
        let receipt = Receipt {
            schema: RECEIPT_SCHEMA.to_string(),
            run_id: "run-1".into(),
            app: "fx".into(),
            started_at: "2026-10-24T10:00:00Z".into(),
            finished_at: "2026-10-24T10:00:05Z".into(),
            duration_ms: 5000,
            root: "/w".into(),
            host: HostInfo::current(),
            compile: CompileRecord::empty("listing"),
            settings: SettingsRecord::defaults(),
            totals: Totals::of(&binaries),
            verdict: judge(&binaries, RULES),
            binaries,
        };
        let text = serde_json::to_string_pretty(&receipt).unwrap();
        let back: Receipt = serde_json::from_str(&text).unwrap();
        assert_eq!(back, receipt);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
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
            "index",
            "package",
            "target",
            "binary",
            "cwd",
            "args",
            "status",
            "exit",
            "timed_out",
            "duration_ms",
            "stdout",
            "stderr",
            "summary",
            "failed_tests",
        ] {
            assert!(
                v["binaries"][0].get(key).is_some(),
                "binary record lacks {key}"
            );
        }
        assert_eq!(v["schema"], "rightkit.run-tests.receipt/1");
        assert_eq!(v["binaries"][0]["status"], "passed");
        assert_eq!(v["verdict"]["exit_code"], 0);
    }

    #[test]
    fn statuses_serialise_as_kebab_case_words() {
        assert_eq!(
            serde_json::to_string(&Status::TimedOut).unwrap(),
            "\"timed-out\""
        );
        assert_eq!(
            serde_json::to_string(&Status::NotRun).unwrap(),
            "\"not-run\""
        );
        for s in [
            Status::Passed,
            Status::Failed,
            Status::TimedOut,
            Status::Error,
            Status::NotRun,
        ] {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
    }

    #[test]
    fn write_places_the_file_atomically_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("rkqa-receipt-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let receipt = Receipt {
            schema: RECEIPT_SCHEMA.to_string(),
            run_id: "r".into(),
            app: "fx".into(),
            started_at: String::new(),
            finished_at: String::new(),
            duration_ms: 0,
            root: String::new(),
            host: HostInfo::current(),
            compile: CompileRecord::empty("listing"),
            settings: SettingsRecord::defaults(),
            binaries: vec![],
            totals: Totals::default(),
            verdict: Verdict::fail(EXIT_SETUP, "x"),
        };
        let path = write(&dir, &receipt).unwrap();
        assert_eq!(path, dir.join(RECEIPT_FILE));
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [RECEIPT_FILE]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
