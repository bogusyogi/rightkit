//! What a libtest binary says about itself: the `test result:` summary, the `running N tests`
//! header and the names of failed tests, read from its captured stdout.
//!
//! The parse is deliberately strict about shape and silent about everything else. A line counts only
//! when it starts with `test result: ` and carries both a passed and a failed count, so a program that
//! prints something similar mid-line, or a custom harness (`harness = false`) that prints nothing, adds
//! no counts. The receipt then records no summary for that binary instead of a made-up zero.
use super::lines::for_each_line;
use serde::{Deserialize, Serialize};
use std::io::BufRead;

/// Longest line the summary parser looks at. Libtest lines are short; a longer one is test output.
const MAX_LINE: usize = 64 * 1024;
/// Failed test names kept in a receipt. A run with more failures than this still fails; the rest are
/// in the binary's stdout file.
pub const MAX_FAILED_NAMES: usize = 200;

/// Counts summed over every `test result:` line a binary printed (a libtest binary prints one; a custom
/// harness may print several).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibtestSummary {
    /// How many `test result:` lines were read.
    pub results: u32,
    /// True when every one of them said `ok`.
    pub ok: bool,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered_out: u64,
}

impl LibtestSummary {
    /// Tests that actually ran.
    pub fn executed(&self) -> u64 {
        self.passed + self.failed
    }

    /// Add `other`'s counts to these (used for the run totals).
    pub fn add(&mut self, other: &LibtestSummary) {
        self.results += other.results;
        self.ok = self.ok && other.ok;
        self.passed += other.passed;
        self.failed += other.failed;
        self.ignored += other.ignored;
        self.measured += other.measured;
        self.filtered_out += other.filtered_out;
    }
}

/// One parsed `test result:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultLine {
    pub ok: bool,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered_out: u64,
}

/// Parse `test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 2 filtered out; finished in 0.00s`.
/// `None` for any other line, and for a `test result:` line without both a passed and a failed count.
pub fn parse_result_line(line: &str) -> Option<ResultLine> {
    let rest = line.strip_prefix("test result: ")?;
    let (status, counts) = rest.split_once(". ")?;
    let ok = match status.trim() {
        "ok" => true,
        "FAILED" => false,
        _ => return None,
    };
    let mut out = ResultLine {
        ok,
        passed: 0,
        failed: 0,
        ignored: 0,
        measured: 0,
        filtered_out: 0,
    };
    let (mut saw_passed, mut saw_failed) = (false, false);
    for part in counts.split(';') {
        let part = part.trim();
        let Some((number, label)) = part.split_once(' ') else {
            continue;
        };
        let Ok(n) = number.parse::<u64>() else {
            continue;
        };
        match label.trim() {
            "passed" => {
                out.passed = n;
                saw_passed = true;
            }
            "failed" => {
                out.failed = n;
                saw_failed = true;
            }
            "ignored" => out.ignored = n,
            "measured" => out.measured = n,
            "filtered out" => out.filtered_out = n,
            _ => {}
        }
    }
    (saw_passed && saw_failed).then_some(out)
}

/// The name in `test some::path ... FAILED`, or `None` for any other line.
pub fn failed_test_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("test ")?;
    let name = rest.strip_suffix(" ... FAILED")?;
    (!name.is_empty()).then_some(name)
}

/// What the stdout of one binary said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// `None` when no `test result:` line was found.
    pub summary: Option<LibtestSummary>,
    pub failed_tests: Vec<String>,
    /// `running N tests` headers summed (what libtest announced it would run).
    pub announced: u64,
}

/// Read a binary's stdout and summarise it. Only the shapes above are looked at; the stream is read once
/// and never held in memory.
pub fn parse_stream<R: BufRead>(reader: R) -> std::io::Result<Parsed> {
    let mut parsed = Parsed::default();
    for_each_line(reader, MAX_LINE, |line| {
        if let Some(r) = parse_result_line(line) {
            let s = parsed.summary.get_or_insert_with(|| LibtestSummary {
                ok: true,
                ..LibtestSummary::default()
            });
            s.results += 1;
            s.ok = s.ok && r.ok;
            s.passed += r.passed;
            s.failed += r.failed;
            s.ignored += r.ignored;
            s.measured += r.measured;
            s.filtered_out += r.filtered_out;
        } else if let Some(name) = failed_test_name(line) {
            if parsed.failed_tests.len() < MAX_FAILED_NAMES
                && !parsed.failed_tests.iter().any(|n| n == name)
            {
                parsed.failed_tests.push(name.to_string());
            }
        } else if let Some(n) = line
            .strip_prefix("running ")
            .and_then(|r| r.strip_suffix(" tests").or_else(|| r.strip_suffix(" test")))
            .and_then(|n| n.parse::<u64>().ok())
        {
            parsed.announced += n;
        }
    })?;
    Ok(parsed)
}
