//! The Harness bridge (ADR-0001 item 8, Sage C8): a time-boxed way to run an app's already-built
//! libtest binary under the runner, outside the broker, one process per test function.
//!
//! The bridge takes no new scenarios. Per bridge entry it:
//!
//! 1. lists the binary (`<bin> --list --format terse`, and `--ignored --list --format terse` to
//!    learn which of those tests are `#[ignore]`d; plain `--list` includes the ignored ones);
//! 2. plans one job per selected test; an ignored test runs only when the entry declares it
//!    ignored-but-required, otherwise it is reported `missing` (`not_run`);
//! 3. runs each job as `<bin> <name> --exact --test-threads=1 --nocapture` (plus `--ignored` for a
//!    declared one), N at a time, with the world marker and run token in the environment so the
//!    runner's "launched from inside cargo test" refusal does not fire (C8);
//! 4. classifies each run from libtest's own stdout (`running N tests`, `test <name> ... <token>`,
//!    `test result: ...`) AND the exit code, and reports one adapter v1 [`JourneyResult`] per test.
//!
//! Fail-closed rules, in the spirit of the module's hard rule:
//!
//! * A run in which libtest did not run exactly one test, or in which the named test's result line
//!   does not appear exactly once, is `errored` ("exact matched 0 or >1 tests"). An `--exact`
//!   filter that matches nothing exits 0; the exit code alone is never trusted.
//! * `passed` needs the result line (or, when `--nocapture` output interleaves with it, the summary),
//!   the summary and exit code 0 to agree. A test that says `ok` with a non-zero exit is `errored`.
//! * An ignored test is never a pass. Undeclared: `missing`. Declared but libtest still ignored it:
//!   `errored`.
//! * libtest reports a test whose harness scenario skipped as `ok`, so `ok` with exit code 0 is a
//!   pass only when the output holds no harness `[<prefix>] SKIP ` or `[<prefix>] NOTRUN ` line
//!   (any prefix: `Harness::with_log_prefix` changes it). Such a test is `skipped`, never `passed`.
//!   A test that failed stays `failed`.
//! * An entry past its removal date is refused with an error naming the app and the date.
//! * A listing that cannot be parsed, a binary that lists zero tests, a selected name that the
//!   binary does not list, and a declaration that disagrees with the binary are errors or `errored`
//!   results, never silently dropped.
//!
//! `missing` is written here because the bridge runs on the runner's side of adapter protocol v1,
//! where the runner alone synthesizes it; the app-side `Reporter` would refuse it.
//!
//! Process spawning goes through [`crate::process::run_owned`] (process group / Job Object, hard
//! deadline); tests inject a fake executor, so the parsing and classification are testable without
//! a binary.
use super::adapter::{JourneyRecord, JourneyResult, Record};
use super::{is_hex_lower, JourneyStatus, Units};
use crate::lock::{acquire, LockOptions};
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, now_iso, tail, Result};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Longest a bridge job may run when the caller does not say otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

// ------------------------------------------------------------------ entry and environment

/// One bridged app: its built libtest binary, the date the bridge for it is removed, and the
/// ignored tests it requires to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeEntry {
    pub app: String,
    /// The already-built libtest binary. The bridge never builds it.
    pub binary: PathBuf,
    /// `YYYY-MM-DD`. The bridge refuses the entry on any day after this one.
    pub removal_date: String,
    /// Tests that are `#[ignore]`d but must run (`--ignored`). Any other ignored test is `missing`.
    pub ignored_required: BTreeSet<String>,
    pub cwd: Option<PathBuf>,
}

impl BridgeEntry {
    fn validate(&self) -> Result<()> {
        if self.app.trim().is_empty() {
            return err("bridge entry has an empty app name");
        }
        if self.binary.as_os_str().is_empty() {
            return err(format!("bridge entry for app {:?} has no binary", self.app));
        }
        for n in &self.ignored_required {
            check_test_name(n)?;
        }
        Ok(())
    }
}

/// What every bridged process carries (Sage C8): the world marker and the run token, so the
/// "launched from inside cargo test" check (world marker and run token absent) does not fire.
/// The runner supplies these; the bridge only passes them on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeEnv {
    pub world_marker_name: String,
    /// Base value. Each process gets `<value>-<test ordinal>` (`-list` for a listing) so a
    /// survivor scan can tell the workers apart.
    pub world_marker_value: String,
    pub run_token_name: String,
    pub run_token_value: String,
    /// Further variables. They may not reuse the marker or token names.
    pub extra: Vec<(String, String)>,
}

fn env_name_ok(name: &str) -> bool {
    !name.is_empty() && !name.contains('=') && !name.contains('\0')
}

impl BridgeEnv {
    pub fn validate(&self) -> Result<()> {
        for (what, name, value) in [
            (
                "world marker",
                &self.world_marker_name,
                &self.world_marker_value,
            ),
            ("run token", &self.run_token_name, &self.run_token_value),
        ] {
            if !env_name_ok(name) {
                return err(format!("{what} variable name {name:?} is not a valid name"));
            }
            if value.trim().is_empty() || value.contains('\0') {
                return err(format!("{what} {name} has an empty or invalid value"));
            }
        }
        if self.world_marker_name == self.run_token_name {
            return err("world marker and run token must use different variable names");
        }
        for (k, v) in &self.extra {
            if !env_name_ok(k) || v.contains('\0') {
                return err(format!("extra variable {k:?} is not valid"));
            }
            if *k == self.world_marker_name || *k == self.run_token_name {
                return err(format!(
                    "extra variable {k} would override the world marker or run token"
                ));
            }
        }
        Ok(())
    }

    fn for_process(&self, suffix: &str) -> Vec<(String, String)> {
        let mut env = vec![
            (
                self.world_marker_name.clone(),
                format!("{}-{suffix}", self.world_marker_value),
            ),
            (self.run_token_name.clone(), self.run_token_value.clone()),
        ];
        env.extend(self.extra.iter().cloned());
        env
    }
}

/// Knobs of one bridge run.
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    /// How many test processes run at once. Zero is refused. The shared pool (worker B) drives
    /// this later.
    pub concurrency: usize,
    /// Hard deadline of each process, listing included.
    pub timeout: Duration,
    /// Run only these tests. A name the binary does not list is an `errored` result.
    pub selected: Option<Vec<String>>,
    /// `YYYY-MM-DD` to compare the removal date with; `None` is today (UTC).
    pub today: Option<String>,
    /// Serialize bridged binaries through a [`crate::lock`] directory for the whole run. The
    /// directory must not be one the app's own `Harness::with_run_lock` uses, or the binary would
    /// wait on the lock its runner holds.
    pub run_lock: Option<(PathBuf, LockOptions)>,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        BridgeOptions {
            concurrency: 1,
            timeout: DEFAULT_TIMEOUT,
            selected: None,
            today: None,
            run_lock: None,
        }
    }
}

// ------------------------------------------------------------------ removal date

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) {
                29
            } else {
                28
            }
        }
    }
}

/// Strict `YYYY-MM-DD` with a real calendar day.
fn parse_day(text: &str) -> Result<(u32, u32, u32)> {
    let shape = text.len() == 10
        && text.bytes().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    if !shape {
        return err(format!("{text:?} is not a YYYY-MM-DD date"));
    }
    // The shape check makes the text ASCII, so these byte ranges are char boundaries.
    let num = |a: usize, b: usize| text[a..b].parse::<u32>().unwrap_or(0);
    let (y, m, d) = (num(0, 4), num(5, 7), num(8, 10));
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return err(format!("{text:?} is not a calendar day"));
    }
    Ok((y, m, d))
}

/// Today's UTC date as `YYYY-MM-DD`.
pub fn today_utc() -> String {
    now_iso().chars().take(10).collect()
}

/// Refuse an entry whose removal date is before `today`. The removal date itself still runs. A
/// removal date or `today` that is not a real `YYYY-MM-DD` day is an error, never "not expired".
pub fn check_removal_date(entry: &BridgeEntry, today: &str) -> Result<()> {
    let removal = parse_day(&entry.removal_date).map_err(|e| {
        crate::util::Error(format!(
            "bridge entry for app {:?} has an invalid removal date: {e}",
            entry.app
        ))
    })?;
    let now = parse_day(today)?;
    if now > removal {
        return err(format!(
            "bridge entry for app {:?} expired on {} (today is {today}): the bridge refuses to run it; convert or delete the app's bridged tests",
            entry.app, entry.removal_date
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ argv and executor

/// A test name the bridge will pass as a libtest filter: non-empty, no surrounding whitespace, and
/// not shaped like an option.
fn check_test_name(name: &str) -> Result<()> {
    if name.is_empty() || name.trim() != name || name.starts_with('-') || name.contains('\0') {
        return err(format!("{name:?} is not a usable test name"));
    }
    Ok(())
}

/// `--list --format terse`, with `--ignored` to list only the ignored tests.
pub fn list_args(ignored_only: bool) -> Vec<String> {
    let mut a = vec!["--list".to_string(), "--format".into(), "terse".into()];
    if ignored_only {
        a.push("--ignored".into());
    }
    a
}

/// `<name> --exact --test-threads=1 --nocapture`, plus `--ignored` for a declared ignored test.
pub fn run_args(name: &str, ignored: bool) -> Vec<String> {
    let mut a = vec![
        name.to_string(),
        "--exact".into(),
        "--test-threads=1".into(),
        "--nocapture".into(),
    ];
    if ignored {
        a.push("--ignored".into());
    }
    a
}

/// One process the bridge wants started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub label: String,
}

/// What came back from one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub started_at: String,
    pub duration_ms: u64,
}

/// How the bridge starts a process. [`owned_exec`] is the real one; tests pass a fake.
pub type Exec<'a> = &'a (dyn Fn(&Invocation) -> Result<RawRun> + Sync);

/// The real executor: [`run_owned`] under `tracker` (process group / Job Object, hard deadline,
/// the whole tree killed on timeout).
pub fn owned_exec(tracker: Tracker) -> impl Fn(&Invocation) -> Result<RawRun> + Sync {
    move |inv: &Invocation| {
        let started_at = now_iso();
        let t0 = Instant::now();
        let out = run_owned(
            &inv.program,
            &inv.args,
            &RunOptions {
                cwd: inv.cwd.clone(),
                env: inv.env.clone(),
                clear_env: false,
                timeout: Some(inv.timeout),
                label: inv.label.clone(),
            },
            &tracker,
        )?;
        Ok(RawRun {
            code: out.code,
            stdout: out.stdout,
            stderr: out.stderr,
            timed_out: out.timed_out,
            started_at,
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }
}

// ------------------------------------------------------------------ parsing `--list`

/// `N tests, M benchmarks` (the pretty summary; singular forms allowed).
fn parse_list_summary(line: &str) -> Option<(usize, usize)> {
    let (a, b) = line.split_once(", ")?;
    let n = a
        .strip_suffix(" tests")
        .or_else(|| a.strip_suffix(" test"))?
        .parse()
        .ok()?;
    let m = b
        .strip_suffix(" benchmarks")
        .or_else(|| b.strip_suffix(" benchmark"))?
        .parse()
        .ok()?;
    Some((n, m))
}

/// Parse `--list --format terse` stdout into test names (benchmarks are counted, not returned).
///
/// Every non-blank line must be `<name>: test`, `<name>: benchmark` or a trailing
/// `N tests, M benchmarks` summary. libtest's terse listing normally has no summary; when one is
/// present its counts must match. Anything else is an error: output the bridge cannot account for
/// is not a listing. A repeated name is an error.
pub fn parse_list(stdout: &str) -> Result<Vec<String>> {
    let mut tests: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut benches = 0usize;
    let mut summary: Option<(usize, usize)> = None;
    for raw in stdout.lines() {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if summary.is_some() {
            return err(format!(
                "listing has output after its summary line: {line:?}"
            ));
        }
        if let Some(s) = parse_list_summary(line) {
            summary = Some(s);
        } else if let Some(name) = line.strip_suffix(": test") {
            if name.is_empty() {
                return err("listing has a test with an empty name");
            }
            if !seen.insert(name.to_string()) {
                return err(format!("listing names test {name:?} twice"));
            }
            tests.push(name.to_string());
        } else if line.strip_suffix(": benchmark").is_some() {
            benches += 1;
        } else {
            return err(format!("unrecognized line in test listing: {line:?}"));
        }
    }
    if let Some((n, b)) = summary {
        if n != tests.len() || b != benches {
            return err(format!(
                "listing summary says {n} tests and {b} benchmarks but the lines hold {} and {benches}",
                tests.len()
            ));
        }
    }
    Ok(tests)
}

// ------------------------------------------------------------------ planning

/// One test process to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    /// Run with `--ignored` (a test the entry declares ignored-but-required).
    pub ignored: bool,
}

/// What the listing and the entry's declarations add up to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub jobs: Vec<Job>,
    /// Ignored tests the entry does not require: reported `missing`, never run.
    pub not_run: Vec<(String, String)>,
    /// Names that cannot run as asked: reported `errored`.
    pub errored: Vec<(String, String)>,
}

/// Combine the full listing, the ignored listing, the entry's declarations and the selection.
/// Output order is by test name. A binary that lists no tests, an ignored list that names tests
/// the full list lacks, and an empty selection are errors.
pub fn plan(
    all: &[String],
    ignored: &[String],
    declared: &BTreeSet<String>,
    selected: Option<&[String]>,
) -> Result<Plan> {
    if all.is_empty() {
        return err("the binary lists zero tests: zero is never a pass");
    }
    let all_set: BTreeSet<&str> = all.iter().map(String::as_str).collect();
    let ignored_set: BTreeSet<&str> = ignored.iter().map(String::as_str).collect();
    if let Some(extra) = ignored_set.iter().find(|n| !all_set.contains(**n)) {
        return err(format!(
            "the ignored listing names {extra:?}, which the full listing does not"
        ));
    }
    let wanted: BTreeSet<String> = match selected {
        Some(s) => {
            if s.is_empty() {
                return err("the selection is empty: zero selected is never a pass");
            }
            for n in s {
                check_test_name(n)?;
            }
            s.iter().cloned().collect()
        }
        None => all.iter().cloned().collect(),
    };
    let mut p = Plan::default();
    for name in &wanted {
        if !all_set.contains(name.as_str()) {
            p.errored.push((
                name.clone(),
                "not listed by the binary's --list: no such test".into(),
            ));
        } else if ignored_set.contains(name.as_str()) {
            if declared.contains(name) {
                p.jobs.push(Job {
                    name: name.clone(),
                    ignored: true,
                });
            } else {
                p.not_run.push((
                    name.clone(),
                    "ignored test the bridge entry does not declare required: not run".into(),
                ));
            }
        } else if declared.contains(name) {
            p.errored.push((
                name.clone(),
                "the entry declares this test ignored-but-required but the binary does not ignore it: fix the entry".into(),
            ));
        } else {
            p.jobs.push(Job {
                name: name.clone(),
                ignored: false,
            });
        }
    }
    if selected.is_none() {
        for d in declared {
            if !all_set.contains(d.as_str()) {
                p.errored.push((
                    d.clone(),
                    "declared ignored-but-required but not listed by the binary: fix the entry"
                        .into(),
                ));
            }
        }
    }
    Ok(p)
}

// ------------------------------------------------------------------ classifying one run

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Ok,
    Failed,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Summary {
    ok: bool,
    passed: u64,
    failed: u64,
    ignored: u64,
    measured: u64,
}

/// `Some(token)` for the bare result words; `None` when `--nocapture` output sits where the word
/// would be (the summary then decides).
fn token_of(rest: &str) -> Option<Token> {
    match rest {
        "ok" => Some(Token::Ok),
        "FAILED" => Some(Token::Failed),
        "ignored" => Some(Token::Ignored),
        r if r.starts_with("ignored, ") => Some(Token::Ignored),
        _ => None,
    }
}

/// `running N test` / `running N tests`.
fn parse_running(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("running ")?;
    rest.strip_suffix(" tests")
        .or_else(|| rest.strip_suffix(" test"))?
        .parse()
        .ok()
}

/// `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s`.
/// An unknown segment makes the whole line unreadable.
fn parse_summary(line: &str) -> Option<Summary> {
    let rest = line.strip_prefix("test result: ")?;
    let (head, counts) = rest.split_once(". ")?;
    let ok = match head {
        "ok" => true,
        "FAILED" => false,
        _ => return None,
    };
    let (mut passed, mut failed, mut ignored, mut measured, mut filtered) =
        (None, None, None, None, None);
    for seg in counts.split("; ") {
        let seg = seg.trim();
        if seg.starts_with("finished in") {
            continue;
        }
        let (n, what) = seg.split_once(' ')?;
        let n: u64 = n.parse().ok()?;
        match what {
            "passed" => passed = Some(n),
            "failed" => failed = Some(n),
            "ignored" => ignored = Some(n),
            "measured" => measured = Some(n),
            "filtered out" => filtered = Some(n),
            _ => return None,
        }
    }
    let _filtered: u64 = filtered?;
    Some(Summary {
        ok,
        passed: passed?,
        failed: failed?,
        ignored: ignored?,
        measured: measured?,
    })
}

struct RunParse {
    running: Vec<usize>,
    /// One entry per `test <name> ... ` line.
    results: Vec<Option<Token>>,
    summaries: Vec<Option<Summary>>,
}

fn parse_run(name: &str, stdout: &str) -> RunParse {
    let prefix = format!("test {name} ... ");
    let mut p = RunParse {
        running: vec![],
        results: vec![],
        summaries: vec![],
    };
    for raw in stdout.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(prefix.as_str()) {
            p.results.push(token_of(rest.trim_end()));
        } else if let Some(n) = parse_running(line.trim()) {
            p.running.push(n);
        } else if line.starts_with("test result: ") {
            p.summaries.push(parse_summary(line.trim_end()));
        }
    }
    p
}

fn describe_exit(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("exit code {c}"),
        None => "no exit code (ended by a signal)".to_string(),
    }
}

fn failure_excerpt(stdout: &str) -> String {
    let lines: Vec<&str> = stdout.lines().collect();
    match lines.iter().position(|l| l.contains("panicked at")) {
        Some(i) => lines[i..(i + 2).min(lines.len())].join(" | "),
        None => tail(stdout.trim(), 400),
    }
}

/// Longest harness line quoted in a skipped result's reason, in characters.
const SKIP_LINE_QUOTE_MAX: usize = 200;

/// Whether `line` opens with a harness outcome marker for a scenario that did not run:
/// `[<prefix>] SKIP ` or `[<prefix>] NOTRUN `. The prefix is `[qa]` by default and
/// `Harness::with_log_prefix` changes it (`[e2e]`), so any bracketed word of 1 to 32 ASCII
/// alphanumerics, `-` or `_` counts.
fn is_not_run_marker(line: &str) -> bool {
    let Some((word, after)) = line.strip_prefix('[').and_then(|r| r.split_once(']')) else {
        return false;
    };
    (1..=32).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && (after.starts_with(" SKIP ") || after.starts_with(" NOTRUN "))
}

/// The first line of `stdout` that is a harness skip or never-started line, bounded for quoting.
/// The line must START with the marker: text that merely mentions one does not count. With
/// `--test-threads=1 --nocapture` libtest has already printed `test <name> ... ` on the line the
/// test's first output continues, so that prefix is dropped before the check.
fn first_not_run_line(name: &str, stdout: &str) -> Option<String> {
    let result_prefix = format!("test {name} ... ");
    stdout.lines().find_map(|raw| {
        let line = raw.trim_end_matches('\r').trim_start();
        let line = line
            .strip_prefix(result_prefix.as_str())
            .map_or(line, str::trim_start);
        is_not_run_marker(line).then(|| {
            let mut quoted: String = line.chars().take(SKIP_LINE_QUOTE_MAX).collect();
            if quoted.len() < line.len() {
                quoted.push_str("...");
            }
            quoted
        })
    })
}

type Verdict = (JourneyStatus, Units, Option<String>);

/// Decide one test's result from its process. Only exit code 0 together with a matching libtest
/// result and summary for exactly one test, and no harness skip line in its output, is `passed`.
pub fn classify_run(name: &str, declared_ignored: bool, raw: &RawRun) -> Verdict {
    let errored =
        |why: String| -> Verdict { (JourneyStatus::Errored, Units::default(), Some(why)) };
    if raw.timed_out {
        return errored(format!(
            "timed out; the process tree was killed. stderr tail: {}",
            tail(raw.stderr.trim(), 300)
        ));
    }
    let exit = describe_exit(raw.code);
    let p = parse_run(name, &raw.stdout);
    match p.running.as_slice() {
        [1] => {}
        [] => {
            return errored(format!(
                "no 'running N tests' line, so this is not libtest output for {name:?} ({exit}). stderr tail: {}",
                tail(raw.stderr.trim(), 300)
            ))
        }
        [n] => {
            return errored(format!(
                "exact matched 0 or >1 tests: libtest ran {n} tests for {name:?}, expected exactly 1 ({exit})"
            ))
        }
        many => {
            return errored(format!(
                "libtest printed {} 'running' lines for {name:?}, expected one",
                many.len()
            ))
        }
    }
    if p.results.len() != 1 {
        return errored(format!(
            "exact matched 0 or >1 tests: the result line for {name:?} appeared {} times, expected exactly once ({exit})",
            p.results.len()
        ));
    }
    let summary = match p.summaries.as_slice() {
        [Some(s)] => *s,
        [None] => {
            return errored(format!(
                "the 'test result:' line for {name:?} is unreadable ({exit})"
            ))
        }
        [] => {
            return errored(format!(
                "no 'test result:' summary for {name:?}: the run did not finish ({exit}). stderr tail: {}",
                tail(raw.stderr.trim(), 300)
            ))
        }
        many => {
            return errored(format!(
                "libtest printed {} summaries for {name:?}, expected one",
                many.len()
            ))
        }
    };
    let total = summary.passed + summary.failed + summary.ignored + summary.measured;
    if total != 1 || summary.measured != 0 {
        return errored(format!(
            "exact matched 0 or >1 tests: the summary for {name:?} counts {} passed, {} failed, {} ignored, {} measured (expected exactly one test, not a benchmark)",
            summary.passed, summary.failed, summary.ignored, summary.measured
        ));
    }
    if summary.ok != (summary.failed == 0) {
        return errored(format!(
            "the summary for {name:?} contradicts itself (status word vs failed count)"
        ));
    }
    let token = if summary.passed == 1 {
        Token::Ok
    } else if summary.failed == 1 {
        Token::Failed
    } else {
        Token::Ignored
    };
    if let Some(Some(line_token)) = p.results.first() {
        if *line_token != token {
            return errored(format!(
                "the result line and the summary for {name:?} disagree"
            ));
        }
    }
    match token {
        Token::Ok => {
            if raw.code == Some(0) {
                // libtest says `ok` for a test whose scenario skipped: that is not a pass.
                if let Some(line) = first_not_run_line(name, &raw.stdout) {
                    return (
                        JourneyStatus::Skipped,
                        Units {
                            total: 1,
                            passed: 0,
                            failed: 0,
                            skipped: 1,
                        },
                        Some(format!(
                            "libtest reported {name:?} ok, but the harness logged {line:?}: a skipped scenario is not a pass (nor is one that never started)"
                        )),
                    );
                }
                (
                    JourneyStatus::Passed,
                    Units {
                        total: 1,
                        passed: 1,
                        failed: 0,
                        skipped: 0,
                    },
                    None,
                )
            } else {
                errored(format!(
                    "libtest reported {name:?} ok but the process ended with {exit}: not a pass. stderr tail: {}",
                    tail(raw.stderr.trim(), 300)
                ))
            }
        }
        Token::Failed => (
            JourneyStatus::Failed,
            Units {
                total: 1,
                passed: 0,
                failed: 1,
                skipped: 0,
            },
            Some(format!("FAILED ({exit}): {}", failure_excerpt(&raw.stdout))),
        ),
        Token::Ignored => {
            if raw.code != Some(0) {
                errored(format!(
                    "libtest reported {name:?} ignored but the process ended with {exit}"
                ))
            } else if declared_ignored {
                errored(format!(
                    "{name:?} is declared ignored-but-required but libtest still ignored it"
                ))
            } else {
                (
                    JourneyStatus::Missing,
                    Units::default(),
                    Some("libtest reported the test ignored: not run".into()),
                )
            }
        }
    }
}

// ------------------------------------------------------------------ running

fn result(
    id: &str,
    status: JourneyStatus,
    started_at: String,
    duration_ms: u64,
    units: Units,
    reason: Option<String>,
) -> JourneyResult {
    let reason = if status == JourneyStatus::Passed {
        None
    } else {
        Some(match reason {
            Some(r) if !r.trim().is_empty() => r,
            _ => "no reason given".to_string(),
        })
    };
    JourneyResult {
        id: id.to_string(),
        status,
        started_at,
        duration_ms,
        units,
        evidence: vec![],
        reason,
    }
}

fn instant_result(id: &str, status: JourneyStatus, reason: String) -> JourneyResult {
    result(id, status, now_iso(), 0, Units::default(), Some(reason))
}

fn invocation(
    entry: &BridgeEntry,
    env: &BridgeEnv,
    opts: &BridgeOptions,
    args: Vec<String>,
    suffix: &str,
    label: String,
) -> Invocation {
    Invocation {
        program: entry.binary.to_string_lossy().into_owned(),
        args,
        env: env.for_process(suffix),
        cwd: entry.cwd.clone(),
        timeout: opts.timeout,
        label,
    }
}

fn list_tests(
    entry: &BridgeEntry,
    env: &BridgeEnv,
    opts: &BridgeOptions,
    exec: Exec<'_>,
    ignored_only: bool,
) -> Result<Vec<String>> {
    let inv = invocation(
        entry,
        env,
        opts,
        list_args(ignored_only),
        "list",
        format!(
            "bridge:{} {}",
            entry.app,
            if ignored_only { "list-ignored" } else { "list" }
        ),
    );
    let raw = exec(&inv)?;
    if raw.timed_out {
        return err(format!(
            "listing {} timed out after {:?}",
            entry.binary.display(),
            opts.timeout
        ));
    }
    if raw.code != Some(0) {
        return err(format!(
            "listing {} ended with {}: {}",
            entry.binary.display(),
            describe_exit(raw.code),
            tail(raw.stderr.trim(), 300)
        ));
    }
    parse_list(&raw.stdout)
}

fn run_one(
    entry: &BridgeEntry,
    env: &BridgeEnv,
    opts: &BridgeOptions,
    job: &Job,
    ordinal: usize,
    exec: Exec<'_>,
) -> JourneyResult {
    let inv = invocation(
        entry,
        env,
        opts,
        run_args(&job.name, job.ignored),
        &ordinal.to_string(),
        format!("bridge:{} {}", entry.app, job.name),
    );
    match exec(&inv) {
        Err(e) => instant_result(
            &job.name,
            JourneyStatus::Errored,
            format!("could not start the test process: {e}"),
        ),
        Ok(raw) => {
            let (status, units, reason) = classify_run(&job.name, job.ignored, &raw);
            result(
                &job.name,
                status,
                raw.started_at,
                raw.duration_ms,
                units,
                reason,
            )
        }
    }
}

fn run_jobs(
    entry: &BridgeEntry,
    env: &BridgeEnv,
    opts: &BridgeOptions,
    jobs: &[Job],
    exec: Exec<'_>,
) -> Vec<JourneyResult> {
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<JourneyResult>>> = Mutex::new(vec![None; jobs.len()]);
    let workers = opts.concurrency.min(jobs.len()).max(1);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= jobs.len() {
                    break;
                }
                let r = run_one(entry, env, opts, &jobs[i], i, exec);
                if let Ok(mut g) = slots.lock() {
                    g[i] = Some(r);
                }
            });
        }
    });
    let slots = slots.into_inner().unwrap_or_default();
    jobs.iter()
        .enumerate()
        .map(|(i, job)| {
            slots.get(i).cloned().flatten().unwrap_or_else(|| {
                instant_result(
                    &job.name,
                    JourneyStatus::Errored,
                    "the bridge worker thread died before reporting".into(),
                )
            })
        })
        .collect()
}

/// Run one bridge entry: refuse it past its removal date, list, plan, run the jobs
/// `opts.concurrency` at a time, and return one result per test, sorted by test name. `Err` means
/// the bridge could not even plan the run (expired entry, bad env, unusable listing, zero tests,
/// run lock timeout); a problem with one test is that test's `errored` result.
pub fn run_entry(
    entry: &BridgeEntry,
    env: &BridgeEnv,
    opts: &BridgeOptions,
    exec: Exec<'_>,
) -> Result<Vec<JourneyResult>> {
    let today = opts.today.clone().unwrap_or_else(today_utc);
    check_removal_date(entry, &today)?;
    entry.validate()?;
    env.validate()?;
    if opts.concurrency == 0 {
        return err("bridge concurrency must be at least 1");
    }
    let _lock = match &opts.run_lock {
        Some((dir, lock_opts)) => {
            if let Some(parent) = dir.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Some(acquire(dir, &format!("bridge:{}", entry.app), lock_opts)?)
        }
        None => None,
    };
    let all = list_tests(entry, env, opts, exec, false)?;
    let ignored = list_tests(entry, env, opts, exec, true)?;
    let planned = plan(
        &all,
        &ignored,
        &entry.ignored_required,
        opts.selected.as_deref(),
    )?;
    let mut results: Vec<JourneyResult> = Vec::new();
    for (name, why) in &planned.errored {
        results.push(instant_result(name, JourneyStatus::Errored, why.clone()));
    }
    for (name, why) in &planned.not_run {
        results.push(instant_result(name, JourneyStatus::Missing, why.clone()));
    }
    results.extend(run_jobs(entry, env, opts, &planned.jobs, exec));
    results.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(results)
}

// ------------------------------------------------------------------ result lines

/// One `results.jsonl` line for a bridged test, in the adapter v1 `journey` record shape (the
/// record type of [`super::adapter`]), without the trailing newline. Refuses what the protocol
/// forbids: a bad `run_id` or `nonce`, units that do not add up, `passed` without a passed unit,
/// any other status without a reason. `missing` is allowed because the bridge is the runner side.
pub fn result_line(run_id: &str, nonce: &str, r: &JourneyResult) -> Result<String> {
    if run_id.trim().is_empty() {
        return err("run_id is empty");
    }
    if !is_hex_lower(nonce, 32) {
        return err("nonce must be 32 lowercase hex characters");
    }
    if r.id.trim().is_empty() {
        return err("a result has an empty id");
    }
    if !r.units.consistent() {
        return err(format!("result {}: units do not add up to total", r.id));
    }
    if r.status == JourneyStatus::Passed && !r.units.supports_pass() {
        return err(format!(
            "result {}: \"passed\" needs units.total >= 1 with none failed or skipped",
            r.id
        ));
    }
    if r.status != JourneyStatus::Passed && r.reason.as_deref().is_none_or(|s| s.trim().is_empty())
    {
        return err(format!(
            "result {}: a reason is required unless passed",
            r.id
        ));
    }
    let rec = Record::Journey(JourneyRecord {
        run_id: run_id.to_string(),
        nonce: nonce.to_string(),
        id: r.id.clone(),
        status: r.status,
        started_at: r.started_at.clone(),
        duration_ms: r.duration_ms,
        units: r.units,
        evidence: r.evidence.clone(),
        reason: r.reason.clone(),
    });
    Ok(serde_json::to_string(&rec)?)
}

/// [`result_line`] for every result: one line each, newline-terminated, in the order given.
pub fn result_lines(run_id: &str, nonce: &str, results: &[JourneyResult]) -> Result<String> {
    let mut out = String::new();
    for r in results {
        out.push_str(&result_line(run_id, nonce, r)?);
        out.push('\n');
    }
    Ok(out)
}

// ------------------------------------------------------------------ fixtures

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    const LIST_4: &str =
        "alpha::passes: test\nalpha::fails: test\nbeta::slow_ok: test\nbeta::ok_two: test\n";

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn entry(removal: &str) -> BridgeEntry {
        BridgeEntry {
            app: "harness-app".into(),
            binary: PathBuf::from("app-tests.exe"),
            removal_date: removal.into(),
            ignored_required: BTreeSet::new(),
            cwd: None,
        }
    }

    fn env() -> BridgeEnv {
        BridgeEnv {
            world_marker_name: "RIGHTKIT_QA_WORLD".into(),
            world_marker_value: "w-0123".into(),
            run_token_name: "RIGHTKIT_QA_RUN_TOKEN".into(),
            run_token_value: "tok-abc".into(),
            extra: vec![],
        }
    }

    fn raw(code: Option<i32>, stdout: &str) -> RawRun {
        RawRun {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
            started_at: "2026-10-23T10:00:00Z".into(),
            duration_ms: 12,
        }
    }

    fn lib_out(name: &str, token: &str, passed: u32, failed: u32, ignored: u32) -> String {
        format!(
            "\nrunning 1 test\ntest {name} ... {token}\n\ntest result: {}. {passed} passed; {failed} failed; {ignored} ignored; 0 measured; 3 filtered out; finished in 0.00s\n\n",
            if failed == 0 { "ok" } else { "FAILED" }
        )
    }

    /// libtest output of one test that printed `printed` and then passed, as `--test-threads=1
    /// --nocapture` writes it: the test's first line continues libtest's own `test <name> ... `
    /// line, and the bare `ok` follows on a line of its own.
    fn lib_out_printing(name: &str, printed: &str) -> String {
        format!(
            "\nrunning 1 test\ntest {name} ... {printed}\nok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s\n\n"
        )
    }

    fn status_of(v: &Verdict) -> JourneyStatus {
        v.0
    }

    // ---- listing

    #[test]
    fn list_of_four_tests_plans_four_jobs() {
        let all = parse_list(LIST_4).unwrap();
        assert_eq!(all.len(), 4);
        let ign = parse_list("").unwrap();
        let p = plan(&all, &ign, &BTreeSet::new(), None).unwrap();
        assert_eq!(p.jobs.len(), 4);
        assert!(p.jobs.iter().all(|j| !j.ignored));
        assert!(p.not_run.is_empty() && p.errored.is_empty());
        assert_eq!(p.jobs[0].name, "alpha::fails"); // sorted by name
    }

    #[test]
    fn list_accepts_benchmarks_and_a_matching_summary() {
        let out = "a::t: test\nb::bench: benchmark\n\n1 test, 1 benchmark\n";
        assert_eq!(parse_list(out).unwrap(), names(&["a::t"]));
        assert!(parse_list("a::t: test\n\n2 tests, 0 benchmarks\n").is_err());
    }

    #[test]
    fn list_refuses_what_it_cannot_account_for() {
        assert!(parse_list("a: test\nwarning: stray\n").is_err());
        assert!(parse_list("a: test\na: test\n").is_err());
        assert!(parse_list(": test\n").is_err());
        assert!(parse_list("1 test, 0 benchmarks\na: test\n").is_err());
    }

    #[test]
    fn list_handles_crlf_and_empty_output() {
        assert_eq!(parse_list("a: test\r\nb: test\r\n").unwrap().len(), 2);
        assert!(parse_list("").unwrap().is_empty());
        // zero tests is refused by the plan, never a pass
        assert!(plan(&[], &[], &BTreeSet::new(), None).is_err());
    }

    #[test]
    fn argv_matches_the_ruling() {
        assert_eq!(list_args(false), ["--list", "--format", "terse"]);
        assert_eq!(
            list_args(true),
            ["--list", "--format", "terse", "--ignored"]
        );
        assert_eq!(
            run_args("a::b", false),
            ["a::b", "--exact", "--test-threads=1", "--nocapture"]
        );
        assert_eq!(
            run_args("a::b", true),
            [
                "a::b",
                "--exact",
                "--test-threads=1",
                "--nocapture",
                "--ignored"
            ]
        );
    }

    // ---- planning and ignored handling

    #[test]
    fn ignored_tests_are_not_run_unless_declared() {
        let all = names(&["a", "b", "c"]);
        let ign = names(&["b", "c"]);
        let declared: BTreeSet<String> = ["c".to_string()].into();
        let p = plan(&all, &ign, &declared, None).unwrap();
        assert_eq!(
            p.jobs,
            vec![
                Job {
                    name: "a".into(),
                    ignored: false
                },
                Job {
                    name: "c".into(),
                    ignored: true
                },
            ]
        );
        assert_eq!(p.not_run.len(), 1);
        assert_eq!(p.not_run[0].0, "b");
        assert!(p.errored.is_empty());
    }

    #[test]
    fn declarations_and_selections_that_disagree_with_the_binary_are_errored() {
        let all = names(&["a", "b"]);
        let ign = names(&["b"]);
        // declared but not ignored, and declared but not listed
        let declared: BTreeSet<String> = ["a".to_string(), "ghost".to_string()].into();
        let p = plan(&all, &ign, &declared, None).unwrap();
        let errored: Vec<&str> = p.errored.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(errored, ["a", "ghost"]);
        assert!(p.jobs.is_empty());
        // a selected name the binary does not list
        let p = plan(
            &all,
            &ign,
            &BTreeSet::new(),
            Some(names(&["a", "nope"]).as_slice()),
        )
        .unwrap();
        assert_eq!(p.jobs.len(), 1);
        assert_eq!(p.errored[0].0, "nope");
        // unusable selections
        assert!(plan(&all, &ign, &BTreeSet::new(), Some(&[])).is_err());
        assert!(plan(
            &all,
            &ign,
            &BTreeSet::new(),
            Some(names(&["--help"]).as_slice())
        )
        .is_err());
        // ignored listing inconsistent with the full listing
        assert!(plan(&all, &names(&["zzz"]), &BTreeSet::new(), None).is_err());
    }

    // ---- classification

    #[test]
    fn ok_with_exit_zero_is_the_only_pass() {
        let v = classify_run("t", false, &raw(Some(0), &lib_out("t", "ok", 1, 0, 0)));
        assert_eq!(status_of(&v), JourneyStatus::Passed);
        assert!(v.1.supports_pass());
        assert!(v.2.is_none());
    }

    const ONE_SKIPPED: Units = Units {
        total: 1,
        passed: 0,
        failed: 0,
        skipped: 1,
    };

    #[test]
    fn ok_with_a_harness_skip_line_is_skipped_not_passed() {
        // The usual shape: the skip is the test's first output, so it continues libtest's line.
        let out = lib_out_printing("t", "[qa] SKIP export loudness: missing command ffmpeg");
        let v = classify_run("t", false, &raw(Some(0), &out));
        assert_eq!(status_of(&v), JourneyStatus::Skipped);
        assert_eq!(v.1, ONE_SKIPPED);
        assert!(v.1.consistent() && !v.1.supports_pass());
        let why = v.2.unwrap();
        assert!(
            why.contains("[qa] SKIP export loudness: missing command ffmpeg")
                && why.contains("a skipped scenario is not a pass"),
            "{why}"
        );
        // Not the first output, after a scenario that did run.
        let later = lib_out_printing(
            "t",
            "[qa] PASS one (3 ms)\n[qa] SKIP two: tier full not selected",
        );
        let v = classify_run("t", false, &raw(Some(0), &later));
        assert_eq!(status_of(&v), JourneyStatus::Skipped);
        assert_eq!(v.1.skipped, 1);
        assert!(v
            .2
            .unwrap()
            .contains("[qa] SKIP two: tier full not selected"));
        // Indented, with CRLF line endings.
        let crlf = lib_out_printing("t", "x\n   [qa] SKIP two: why").replace('\n', "\r\n");
        let v = classify_run("t", false, &raw(Some(0), &crlf));
        assert_eq!(status_of(&v), JourneyStatus::Skipped);
        assert!(v
            .2
            .unwrap()
            .ends_with("is not a pass (nor is one that never started)"));
    }

    #[test]
    fn a_custom_log_prefix_is_still_a_skip() {
        for prefix in ["[e2e]", "[genright-e2e]", "[my_app2]", "[A]"] {
            let line = format!("{prefix} SKIP scenario: missing env KEY");
            let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", &line)));
            assert_eq!(status_of(&v), JourneyStatus::Skipped, "{prefix}");
            assert_eq!(v.1.skipped, 1, "{prefix}");
            assert!(v.2.unwrap().contains(&line), "{prefix}");
        }
        // A 32 character word is the longest accepted prefix.
        let longest = format!("[{}] SKIP x: y", "a".repeat(32));
        let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", &longest)));
        assert_eq!(status_of(&v), JourneyStatus::Skipped);
    }

    #[test]
    fn a_scenario_that_never_started_is_not_a_pass_either() {
        for line in [
            "[qa] NOTRUN login: never started",
            "[e2e] NOTRUN login (4 ms)",
        ] {
            let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", line)));
            assert_eq!(status_of(&v), JourneyStatus::Skipped, "{line}");
            assert_eq!(v.1, ONE_SKIPPED, "{line}");
        }
    }

    #[test]
    fn a_pass_line_or_text_that_only_mentions_a_skip_is_still_a_pass() {
        for printed in [
            "[qa] PASS export loudness (12 ms)",
            "[e2e] PASS login (4 ms)\n[qa] PASS logout (5 ms)",
            // Does not START with the marker.
            "note: [qa] SKIP inside text",
            "see [qa] SKIP in the log\n    prefix [e2e] SKIP too",
            // Not the marker: no space after SKIP, an invalid or empty or overlong prefix word.
            "[qa] SKIPPED x",
            "[qa] SKIP",
            "[q a] SKIP x: y",
            "[] SKIP x: y",
            "qa SKIP x: y",
            "[qa]SKIP x: y",
        ] {
            let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", printed)));
            assert_eq!(status_of(&v), JourneyStatus::Passed, "{printed}");
            assert_eq!(v.1.passed, 1, "{printed}");
            assert!(v.1.supports_pass() && v.2.is_none(), "{printed}");
        }
        let too_long = format!("[{}] SKIP x: y", "a".repeat(33));
        let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", &too_long)));
        assert_eq!(status_of(&v), JourneyStatus::Passed);
        // And the plain pass without any output stays a pass.
        let v = classify_run("t", false, &raw(Some(0), &lib_out("t", "ok", 1, 0, 0)));
        assert_eq!(status_of(&v), JourneyStatus::Passed);
    }

    #[test]
    fn a_failed_test_stays_failed_and_a_bad_exit_stays_errored_whatever_it_printed() {
        let skip = "[qa] SKIP two: why";
        let failed = format!(
            "\nrunning 1 test\ntest t ... {skip}\nFAILED\n\nfailures:\n\n---- t stdout ----\nthread 't' panicked at a.rs:1:1:\nboom\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        );
        let v = classify_run("t", false, &raw(Some(101), &failed));
        assert_eq!(status_of(&v), JourneyStatus::Failed);
        assert_eq!(v.1.failed, 1);
        assert_eq!(v.1.skipped, 0);
        // `ok` with a non-zero exit is errored as before, a skip line or not.
        let ok = lib_out_printing("t", skip);
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(101), &ok))),
            JourneyStatus::Errored
        );
        assert_eq!(
            status_of(&classify_run("t", false, &raw(None, &ok))),
            JourneyStatus::Errored
        );
    }

    #[test]
    fn the_quoted_skip_line_is_bounded() {
        let long = format!("[qa] SKIP big: {}", "x".repeat(5000));
        let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", &long)));
        assert_eq!(status_of(&v), JourneyStatus::Skipped);
        let why = v.2.unwrap();
        assert!(why.len() < 500 && why.contains("..."), "{}", why.len());
        // Multi-byte text is cut on a character boundary.
        let wide = format!("[qa] SKIP big: {}", "é".repeat(500));
        let v = classify_run("t", false, &raw(Some(0), &lib_out_printing("t", &wide)));
        assert_eq!(status_of(&v), JourneyStatus::Skipped);
    }

    #[test]
    fn exact_matching_zero_tests_and_exiting_zero_is_errored() {
        let out = "\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s\n\n";
        let v = classify_run("t", false, &raw(Some(0), out));
        assert_eq!(status_of(&v), JourneyStatus::Errored);
        assert!(v.2.unwrap().contains("exact matched 0 or >1 tests"));
    }

    #[test]
    fn more_than_one_test_or_a_repeated_result_line_is_errored() {
        let two = "\nrunning 2 tests\ntest t ... ok\ntest u ... ok\n\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let v = classify_run("t", false, &raw(Some(0), two));
        assert_eq!(status_of(&v), JourneyStatus::Errored);
        let twice = "\nrunning 1 test\ntest t ... ok\ntest t ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let v = classify_run("t", false, &raw(Some(0), twice));
        assert_eq!(status_of(&v), JourneyStatus::Errored);
        assert!(v.2.unwrap().contains("exact matched 0 or >1 tests"));
        // the summary alone cannot stand in for a missing result line
        let no_line = "\nrunning 1 test\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), no_line))),
            JourneyStatus::Errored
        );
    }

    #[test]
    fn a_panic_is_failed_with_the_message() {
        let out = "\nrunning 1 test\ntest t ... FAILED\n\nfailures:\n\n---- t stdout ----\nthread 't' panicked at src/lib.rs:9:5:\nboom\n\nfailures:\n    t\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let v = classify_run("t", false, &raw(Some(101), out));
        assert_eq!(status_of(&v), JourneyStatus::Failed);
        assert_eq!(v.1.failed, 1);
        assert!(v.2.as_deref().unwrap().contains("boom"));
        // FAILED stays failed even if the exit code is wrong
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), out))),
            JourneyStatus::Failed
        );
    }

    #[test]
    fn ok_with_a_nonzero_exit_or_no_summary_or_a_timeout_is_errored() {
        let ok = lib_out("t", "ok", 1, 0, 0);
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(101), &ok))),
            JourneyStatus::Errored
        );
        assert_eq!(
            status_of(&classify_run("t", false, &raw(None, &ok))),
            JourneyStatus::Errored
        );
        let cut = "\nrunning 1 test\ntest t ... ";
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), cut))),
            JourneyStatus::Errored
        );
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), ""))),
            JourneyStatus::Errored
        );
        let mut slow = raw(None, &ok);
        slow.timed_out = true;
        assert_eq!(
            status_of(&classify_run("t", false, &slow)),
            JourneyStatus::Errored
        );
    }

    #[test]
    fn nocapture_output_in_the_result_line_defers_to_the_summary() {
        let out = "\nrunning 1 test\ntest t ... hello from the test\nok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), out))),
            JourneyStatus::Passed
        );
        // but a disagreeing bare word is errored
        let bad = "\nrunning 1 test\ntest t ... FAILED\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), bad))),
            JourneyStatus::Errored
        );
    }

    #[test]
    fn an_ignored_test_is_never_a_pass() {
        let out = lib_out("t", "ignored", 0, 0, 1);
        let v = classify_run("t", false, &raw(Some(0), &out));
        assert_eq!(status_of(&v), JourneyStatus::Missing);
        assert_eq!(v.1.total, 0);
        // declared required but still ignored: errored
        assert_eq!(
            status_of(&classify_run("t", true, &raw(Some(0), &out))),
            JourneyStatus::Errored
        );
        // a declared test that really ran with --ignored passes
        assert_eq!(
            status_of(&classify_run(
                "t",
                true,
                &raw(Some(0), &lib_out("t", "ok", 1, 0, 0))
            )),
            JourneyStatus::Passed
        );
        // `ignored, <reason>` is still ignored
        let with_reason = lib_out("t", "ignored, needs a GUI", 0, 0, 1);
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), &with_reason))),
            JourneyStatus::Missing
        );
    }

    #[test]
    fn a_benchmark_or_an_unreadable_summary_is_errored() {
        let bench = "\nrunning 1 test\ntest t ... bench: 1 ns/iter\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 1 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), bench))),
            JourneyStatus::Errored
        );
        let odd = "\nrunning 1 test\ntest t ... ok\n\ntest result: ok. 1 passed; 0 failed; 7 weird; 0 measured; 0 filtered out\n";
        assert_eq!(
            status_of(&classify_run("t", false, &raw(Some(0), odd))),
            JourneyStatus::Errored
        );
    }

    // ---- removal date

    #[test]
    fn an_expired_entry_is_refused_naming_the_app_and_the_date() {
        let e = entry("2026-10-01");
        let msg = check_removal_date(&e, "2026-10-02")
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("harness-app") && msg.contains("2026-10-01"),
            "{msg}"
        );
        // the removal date itself still runs, and earlier days do
        assert!(check_removal_date(&e, "2026-10-01").is_ok());
        assert!(check_removal_date(&e, "2026-09-30").is_ok());
        // a year boundary and a leap day compare correctly
        assert!(check_removal_date(&entry("2026-12-31"), "2027-01-01").is_err());
        assert!(check_removal_date(&entry("2028-02-29"), "2028-02-29").is_ok());
        // a date that is not a real day is an error, never "not expired"
        assert!(check_removal_date(&entry("2027-02-29"), "2026-01-01").is_err());
        assert!(check_removal_date(&entry("soon"), "2026-01-01").is_err());
        assert!(check_removal_date(&e, "2026-13-01").is_err());
    }

    #[test]
    fn today_is_a_date() {
        let t = today_utc();
        assert!(parse_day(&t).is_ok(), "{t}");
    }

    // ---- environment

    #[test]
    fn env_carries_the_marker_and_token_and_refuses_overrides() {
        let mut e = env();
        e.validate().unwrap();
        let v = e.for_process("3");
        assert!(v.contains(&("RIGHTKIT_QA_WORLD".into(), "w-0123-3".into())));
        assert!(v.contains(&("RIGHTKIT_QA_RUN_TOKEN".into(), "tok-abc".into())));
        e.extra.push(("RIGHTKIT_QA_WORLD".into(), "x".into()));
        assert!(e.validate().is_err());
        let mut e = env();
        e.run_token_value = " ".into();
        assert!(e.validate().is_err());
        let mut e = env();
        e.world_marker_name = "A=B".into();
        assert!(e.validate().is_err());
    }

    // ---- end to end with a fake executor

    /// A fake libtest binary: four tests, `alpha::fails` panics, `beta::slow_ok` and the rest pass.
    fn fake_binary(inv: &Invocation) -> RawRun {
        if inv.args.first().map(String::as_str) == Some("--list") {
            return raw(
                Some(0),
                if inv.args.iter().any(|a| a == "--ignored") {
                    ""
                } else {
                    LIST_4
                },
            );
        }
        let name = inv.args[0].as_str();
        match name {
            "alpha::fails" => raw(
                Some(101),
                &format!("\nrunning 1 test\ntest {name} ... FAILED\n\nfailures:\n\n---- {name} stdout ----\nthread 'x' panicked at a.rs:1:1:\nno\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s\n"),
            ),
            _ => raw(Some(0), &lib_out(name, "ok", 1, 0, 0)),
        }
    }

    #[test]
    fn four_tests_run_as_four_processes_and_results_are_collected() {
        let calls: Mutex<Vec<Invocation>> = Mutex::new(vec![]);
        let exec = |inv: &Invocation| -> Result<RawRun> {
            calls.lock().unwrap().push(inv.clone());
            Ok(fake_binary(inv))
        };
        let opts = BridgeOptions {
            concurrency: 3,
            today: Some("2026-10-23".into()),
            ..BridgeOptions::default()
        };
        let results = run_entry(&entry("2026-12-31"), &env(), &opts, &exec).unwrap();
        assert_eq!(results.len(), 4);
        let by_id = |id: &str| results.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by_id("alpha::passes").status, JourneyStatus::Passed);
        assert_eq!(by_id("beta::ok_two").status, JourneyStatus::Passed);
        assert_eq!(by_id("beta::slow_ok").status, JourneyStatus::Passed);
        assert_eq!(by_id("alpha::fails").status, JourneyStatus::Failed);
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);

        let calls = calls.into_inner().unwrap();
        // two listings plus one process per test
        assert_eq!(calls.len(), 2 + 4);
        let runs: Vec<&Invocation> = calls.iter().filter(|c| c.args[0] != "--list").collect();
        assert_eq!(runs.len(), 4);
        let mut markers = BTreeSet::new();
        for c in &calls {
            assert!(c.env.iter().any(|(k, _)| k == "RIGHTKIT_QA_RUN_TOKEN"));
            let m = c
                .env
                .iter()
                .find(|(k, _)| k == "RIGHTKIT_QA_WORLD")
                .unwrap();
            assert!(m.1.starts_with("w-0123-"));
            markers.insert(m.1.clone());
        }
        assert_eq!(
            markers.len(),
            5,
            "list processes share one marker, each test has its own"
        );
        for c in runs {
            assert!(c.args.contains(&"--exact".to_string()));
            assert!(c.args.contains(&"--test-threads=1".to_string()));
            assert!(!c.args.contains(&"--ignored".to_string()));
        }
    }

    #[test]
    fn an_expired_entry_launches_nothing() {
        let calls = AtomicUsize::new(0);
        let exec = |inv: &Invocation| -> Result<RawRun> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(fake_binary(inv))
        };
        let opts = BridgeOptions {
            today: Some("2027-01-01".into()),
            ..BridgeOptions::default()
        };
        let msg = run_entry(&entry("2026-12-31"), &env(), &opts, &exec)
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("harness-app") && msg.contains("2026-12-31"),
            "{msg}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_name_matching_zero_tests_is_errored_and_an_ignored_one_is_not_run() {
        // The binary lists `ghost` as a test but `--exact` finds nothing (e.g. a cfg'd-out test),
        // and `skipped_gui` is ignored and undeclared.
        let exec = |inv: &Invocation| -> Result<RawRun> {
            if inv.args[0] == "--list" {
                return Ok(raw(
                    Some(0),
                    if inv.args.iter().any(|a| a == "--ignored") {
                        "skipped_gui: test\n"
                    } else {
                        "ghost: test\nskipped_gui: test\nreal: test\n"
                    },
                ));
            }
            if inv.args[0] == "ghost" {
                return Ok(raw(
                    Some(0),
                    "\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s\n",
                ));
            }
            Ok(raw(Some(0), &lib_out(&inv.args[0], "ok", 1, 0, 0)))
        };
        let opts = BridgeOptions {
            today: Some("2026-10-23".into()),
            ..BridgeOptions::default()
        };
        let results = run_entry(&entry("2026-12-31"), &env(), &opts, &exec).unwrap();
        let st = |id: &str| results.iter().find(|r| r.id == id).unwrap().status;
        assert_eq!(st("ghost"), JourneyStatus::Errored);
        assert_eq!(st("skipped_gui"), JourneyStatus::Missing);
        assert_eq!(st("real"), JourneyStatus::Passed);
    }

    #[test]
    fn a_declared_ignored_test_runs_with_the_ignored_flag() {
        let exec = |inv: &Invocation| -> Result<RawRun> {
            if inv.args[0] == "--list" {
                return Ok(raw(
                    Some(0),
                    if inv.args.iter().any(|a| a == "--ignored") {
                        "needs_gui: test\n"
                    } else {
                        "needs_gui: test\nplain: test\n"
                    },
                ));
            }
            let want_ignored = inv.args[0] == "needs_gui";
            assert_eq!(inv.args.contains(&"--ignored".to_string()), want_ignored);
            Ok(raw(Some(0), &lib_out(&inv.args[0], "ok", 1, 0, 0)))
        };
        let mut e = entry("2026-12-31");
        e.ignored_required.insert("needs_gui".into());
        let opts = BridgeOptions {
            today: Some("2026-10-23".into()),
            ..BridgeOptions::default()
        };
        let results = run_entry(&e, &env(), &opts, &exec).unwrap();
        assert!(results.iter().all(|r| r.status == JourneyStatus::Passed));
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn a_failed_launch_or_a_broken_listing_is_never_a_pass() {
        let opts = BridgeOptions {
            today: Some("2026-10-23".into()),
            ..BridgeOptions::default()
        };
        let broken_list = |_: &Invocation| -> Result<RawRun> { Ok(raw(Some(101), "")) };
        assert!(run_entry(&entry("2026-12-31"), &env(), &opts, &broken_list).is_err());
        let empty_list = |_: &Invocation| -> Result<RawRun> { Ok(raw(Some(0), "")) };
        assert!(run_entry(&entry("2026-12-31"), &env(), &opts, &empty_list).is_err());
        let no_start = |inv: &Invocation| -> Result<RawRun> {
            if inv.args[0] == "--list" {
                Ok(fake_binary(inv))
            } else {
                err("spawn failed")
            }
        };
        let results = run_entry(&entry("2026-12-31"), &env(), &opts, &no_start).unwrap();
        assert!(results.iter().all(|r| r.status == JourneyStatus::Errored));
        let zero = BridgeOptions {
            concurrency: 0,
            today: Some("2026-10-23".into()),
            ..BridgeOptions::default()
        };
        assert!(run_entry(&entry("2026-12-31"), &env(), &zero, &no_start).is_err());
    }

    #[test]
    fn a_skipped_scenario_surfaces_as_a_skipped_result_that_the_adapter_records() {
        let exec = |inv: &Invocation| -> Result<RawRun> {
            if inv.args.first().map(String::as_str) == Some("beta::ok_two") {
                let out = lib_out_printing("beta::ok_two", "[e2e] SKIP login: missing env KEY");
                return Ok(raw(Some(0), &out));
            }
            Ok(fake_binary(inv))
        };
        let opts = BridgeOptions {
            today: Some("2026-10-23".into()),
            ..BridgeOptions::default()
        };
        let results = run_entry(&entry("2026-12-31"), &env(), &opts, &exec).unwrap();
        let by_id = |id: &str| results.iter().find(|r| r.id == id).unwrap();
        let skipped = by_id("beta::ok_two");
        assert_eq!(skipped.status, JourneyStatus::Skipped);
        assert_eq!(skipped.units, ONE_SKIPPED);
        assert!(skipped
            .reason
            .as_deref()
            .unwrap()
            .contains("[e2e] SKIP login"));
        assert_eq!(by_id("alpha::passes").status, JourneyStatus::Passed);
        // The result line is accepted by the protocol check and reads back as skipped.
        let lines = result_lines("run-1", NONCE, &results).unwrap();
        let line = lines
            .lines()
            .find(|l| l.contains("beta::ok_two"))
            .expect("a line for the skipped test");
        match serde_json::from_str::<Record>(line).unwrap() {
            Record::Journey(j) => {
                assert_eq!(j.status, JourneyStatus::Skipped);
                assert_eq!(j.units, ONE_SKIPPED);
                assert!(j.reason.unwrap().contains("not a pass"));
            }
            other => panic!("not a journey record: {other:?}"),
        }
    }

    // ---- result lines

    #[test]
    fn result_lines_use_the_adapter_journey_record_shape() {
        let v = classify_run("t", false, &raw(Some(0), &lib_out("t", "ok", 1, 0, 0)));
        let r = result("t", v.0, "2026-10-23T10:00:00Z".into(), 12, v.1, v.2);
        let line = result_line("run-1", NONCE, &r).unwrap();
        assert!(!line.contains('\n'));
        let rec: Record = serde_json::from_str(&line).unwrap();
        match rec {
            Record::Journey(j) => {
                assert_eq!(j.id, "t");
                assert_eq!(j.status, JourneyStatus::Passed);
                assert_eq!(j.run_id, "run-1");
                assert_eq!(j.nonce, NONCE);
                assert_eq!(j.duration_ms, 12);
                assert!(j.reason.is_none());
            }
            other => panic!("not a journey record: {other:?}"),
        }
        let many = result_lines("run-1", NONCE, &[r.clone(), r]).unwrap();
        assert_eq!(many.lines().count(), 2);
    }

    #[test]
    fn result_lines_refuse_what_the_protocol_forbids() {
        let good = result(
            "t",
            JourneyStatus::Passed,
            "2026-10-23T10:00:00Z".into(),
            1,
            Units {
                total: 1,
                passed: 1,
                failed: 0,
                skipped: 0,
            },
            None,
        );
        assert!(result_line("", NONCE, &good).is_err());
        assert!(result_line("run-1", "short", &good).is_err());
        let mut passed_without_units = good.clone();
        passed_without_units.units = Units::default();
        assert!(result_line("run-1", NONCE, &passed_without_units).is_err());
        let mut no_reason = instant_result("t", JourneyStatus::Failed, "x".into());
        no_reason.reason = None;
        assert!(result_line("run-1", NONCE, &no_reason).is_err());
        // missing is allowed on the runner side, with a reason
        let missing = instant_result("t", JourneyStatus::Missing, "ignored".into());
        assert!(result_line("run-1", NONCE, &missing).is_ok());
    }
}
